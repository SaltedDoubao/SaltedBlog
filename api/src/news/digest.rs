//! 日报生成编排：候选选稿 → LLM 双语整理 → 构建 Markdown → 创建/覆盖文章 → job 落库
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use uuid::Uuid;

use crate::entities::{digest_jobs, news_items, posts};
use crate::news::{llm, load_settings, local_date_string, ranker, seed, NewsSettings};
use crate::render::prepare_post_content;
use crate::state::AppState;

fn digest_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub fn digest_slug(date: &str) -> String {
    format!("ai-daily-{date}")
}

pub fn digest_title(lang: &str, date: &str) -> String {
    match lang {
        "en" => format!("AI Frontier Daily | {date}"),
        _ => format!("AI 前沿日报 | {date}"),
    }
}

/// 生成（或强制重新生成）当日日报。
/// `force=false`（定时路径）：当日已存在任务则跳过；`force=true`（手动路径）：新建任务并覆盖当日文章。
pub async fn generate(
    state: &AppState,
    trigger: &str,
    force: bool,
) -> anyhow::Result<digest_jobs::Model> {
    let Ok(_guard) = digest_lock().try_lock() else {
        anyhow::bail!("日报生成已在进行中");
    };

    let db = state.db();
    let settings = load_settings(&db).await?;
    let date = local_date_string(state.cfg.stats_tz_offset_hours);

    if !force {
        let existing = digest_jobs::Entity::find()
            .filter(digest_jobs::Column::DigestDate.eq(&date))
            .one(&db)
            .await?;
        if let Some(job) = existing {
            anyhow::bail!(
                "当日（{date}）已存在日报任务（状态 {}），如需重新生成请使用强制模式",
                job.status
            );
        }
    }

    let job = digest_jobs::ActiveModel {
        digest_date: Set(date.clone()),
        trigger: Set(trigger.to_string()),
        status: Set(digest_jobs::STATUS_RUNNING.to_string()),
        started_at: Set(Utc::now().into()),
        ..Default::default()
    }
    .insert(&db)
    .await?;

    match run_generation(state, &db, &settings, &date).await {
        Ok(outcome) => {
            let mut model: digest_jobs::ActiveModel = job.into();
            model.status = Set(digest_jobs::STATUS_SUCCESS.to_string());
            model.raw_count = Set(outcome.raw_count as i32);
            model.selected_count = Set(outcome.selected_count as i32);
            model.llm_model = Set(Some(settings.llm_model.clone()));
            model.result_json = Set(serde_json::to_string(&outcome.doc).ok());
            model.post_id_zh = Set(Some(outcome.post_id_zh));
            model.post_id_en = Set(Some(outcome.post_id_en));
            model.error_message = Set(None);
            model.finished_at = Set(Some(Utc::now().into()));
            Ok(model.update(&db).await?)
        }
        Err(e) => {
            let message: String = e.to_string().chars().take(2000).collect();
            let mut model: digest_jobs::ActiveModel = job.into();
            model.status = Set(digest_jobs::STATUS_FAILED.to_string());
            model.error_message = Set(Some(message.clone()));
            model.finished_at = Set(Some(Utc::now().into()));
            let saved = model.update(&db).await?;
            tracing::warn!("digest generation failed for {date}: {message}");
            Ok(saved)
        }
    }
}

struct GenerationOutcome {
    doc: llm::DigestDoc,
    raw_count: usize,
    selected_count: usize,
    post_id_zh: i32,
    post_id_en: i32,
}

async fn run_generation(
    state: &AppState,
    db: &DatabaseConnection,
    settings: &NewsSettings,
    date: &str,
) -> anyhow::Result<GenerationOutcome> {
    // LLM 配置检查
    if settings.llm_base_url.is_empty() || settings.llm_model.is_empty() {
        anyhow::bail!("LLM 未配置：请在后台「情报管理」填写 base_url 与模型名");
    }
    if state.cfg.news_llm_api_key.is_empty() {
        anyhow::bail!("LLM 未配置：环境变量 NEWS_LLM_API_KEY 为空");
    }

    // 选稿
    let pool = ranker::load_candidates(db).await?;
    if pool.selected.is_empty() {
        anyhow::bail!("时间窗内没有候选情报（请先确认信源已启用并完成采集）");
    }

    // LLM 整理
    let system_prompt = build_system_prompt(&settings.llm_extra_prompt);
    let user_prompt = build_user_prompt(date, &pool.selected);
    let raw_output = llm::chat(&llm::LlmRequest {
        base_url: &settings.llm_base_url,
        api_key: &state.cfg.news_llm_api_key,
        model: &settings.llm_model,
        system_prompt: &system_prompt,
        user_prompt: &user_prompt,
    })
    .await?;

    let mut doc = llm::parse_digest_doc(&raw_output).ok_or_else(|| {
        let head: String = raw_output.chars().take(300).collect();
        anyhow::anyhow!("LLM 输出无法解析为日报 JSON：{head}")
    })?;
    doc.date = date.to_string();
    doc.title_zh = digest_title("zh", date);
    doc.title_en = digest_title("en", date);

    // 构建双语文章
    let category_id = seed::ensure_digest_category_id(db).await?;
    let slug = digest_slug(date);
    let markdown_zh = build_markdown(&doc, "zh", pool.raw_count);
    let markdown_en = build_markdown(&doc, "en", pool.raw_count);

    let group_id = existing_group_id(db, &slug)
        .await?
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let post_zh = upsert_post(
        state,
        db,
        UpsertPost {
            lang: "zh",
            slug: &slug,
            title: &doc.title_zh,
            summary: &doc.summary_zh,
            markdown: &markdown_zh,
            category_id,
            group_id: &group_id,
            publish: settings.auto_publish,
        },
    )
    .await?;
    let post_en = upsert_post(
        state,
        db,
        UpsertPost {
            lang: "en",
            slug: &slug,
            title: &doc.title_en,
            summary: &doc.summary_en,
            markdown: &markdown_en,
            category_id,
            group_id: &group_id,
            publish: settings.auto_publish,
        },
    )
    .await?;

    // 消费候选：pending → processed
    let ids: Vec<i32> = pool.selected.iter().map(|c| c.item.id).collect();
    news_items::Entity::update_many()
        .col_expr(
            news_items::Column::Status,
            sea_orm::sea_query::Expr::value(news_items::STATUS_PROCESSED),
        )
        .filter(
            Condition::all()
                .add(news_items::Column::Id.is_in(ids.clone()))
                .add(news_items::Column::Status.eq(news_items::STATUS_PENDING)),
        )
        .exec(db)
        .await?;

    Ok(GenerationOutcome {
        raw_count: pool.raw_count,
        selected_count: ids.len(),
        doc,
        post_id_zh: post_zh.id,
        post_id_en: post_en.id,
    })
}

async fn existing_group_id(db: &DatabaseConnection, slug: &str) -> anyhow::Result<Option<String>> {
    let found = posts::Entity::find()
        .filter(posts::Column::Slug.eq(slug))
        .one(db)
        .await?;
    Ok(found.map(|p| p.group_id))
}

struct UpsertPost<'a> {
    lang: &'a str,
    slug: &'a str,
    title: &'a str,
    summary: &'a str,
    markdown: &'a str,
    category_id: i32,
    group_id: &'a str,
    publish: bool,
}

/// 创建或覆盖日报文章：覆盖时保留 view_count / created_at / published_at / group_id
async fn upsert_post(
    state: &AppState,
    db: &DatabaseConnection,
    input: UpsertPost<'_>,
) -> anyhow::Result<posts::Model> {
    let content = prepare_post_content(&state.jieba, input.title, &[], input.markdown)
        .map_err(|e| anyhow::anyhow!("render digest post failed: {e}"))?;
    let now = Utc::now();
    let summary = normalize_summary(input.summary);

    let existing = posts::Entity::find()
        .filter(
            Condition::all()
                .add(posts::Column::Lang.eq(input.lang))
                .add(posts::Column::Slug.eq(input.slug)),
        )
        .one(db)
        .await?;

    let saved = match existing {
        Some(row) => {
            let published_at = if input.publish {
                row.published_at.or_else(|| Some(now.into()))
            } else {
                row.published_at
            };
            let status = if input.publish {
                posts::STATUS_PUBLISHED.to_string()
            } else {
                row.status.clone()
            };
            let mut model: posts::ActiveModel = row.into();
            model.title = Set(input.title.to_string());
            model.summary = Set(summary);
            model.content_md = Set(input.markdown.to_string());
            model.content_html = Set(content.html);
            model.toc_json = Set(Some(content.toc_json));
            model.search_text = Set(content.search_text);
            model.status = Set(status);
            model.category_id = Set(Some(input.category_id));
            model.updated_at = Set(now.into());
            model.published_at = Set(published_at);
            model.update(db).await?
        }
        None => {
            let status = if input.publish {
                posts::STATUS_PUBLISHED
            } else {
                posts::STATUS_DRAFT
            };
            posts::ActiveModel {
                group_id: Set(input.group_id.to_string()),
                lang: Set(input.lang.to_string()),
                slug: Set(input.slug.to_string()),
                title: Set(input.title.to_string()),
                summary: Set(summary),
                cover: Set(None),
                content_md: Set(input.markdown.to_string()),
                content_html: Set(content.html),
                toc_json: Set(Some(content.toc_json)),
                search_text: Set(content.search_text),
                status: Set(status.to_string()),
                category_id: Set(Some(input.category_id)),
                series_id: Set(None),
                series_order: Set(None),
                view_count: Set(0),
                created_at: Set(now.into()),
                updated_at: Set(now.into()),
                published_at: Set(input.publish.then(|| now.into())),
                ..Default::default()
            }
            .insert(db)
            .await?
        }
    };
    Ok(saved)
}

fn normalize_summary(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(crate::news::normalize::truncate_chars(trimmed, 300))
}

// ---------- Prompt 构建 ----------

fn build_system_prompt(extra: &str) -> String {
    let mut prompt = r#"你是一名科技情报编辑，负责把多信源的原始资讯整理成一份中英双语的《AI 前沿日报》。

要求：
1. 合并重复或相似的信息：不同来源报道同一事件只保留一条（摘要中可提及多来源佐证）。
2. 从候选中挑选最值得关注的 8～15 条，按主题分为 2～5 个分节（如：模型与研究 / 开源项目 / 行业动态 / 社区热议，可按当日内容调整命名）。
3. 每条产出中文与英文两个版本的标题、摘要（1～2 句）与「为什么值得关注」（1 句）。英文要地道，不要机械直译；中文保留专有名词原文（如模型名、库名）。
4. importance 取 1～5：5=今日最重要（全篇至多 2 条），1=普通。
5. tags 为 1～3 个简短英文小写标签（如 "llm"、"agents"、"rust"）。
6. 不得编造输入中不存在的信息；source 与 url 必须原样取自输入。
7. 只输出合法 JSON：不要 Markdown 代码块包裹，不要输出任何解释文字。

输出 JSON schema：
{
  "date": "YYYY-MM-DD",
  "title_zh": "标题", "title_en": "Title",
  "summary_zh": "今日整体综述（2～3 句）", "summary_en": "Overall summary (2-3 sentences)",
  "sections": [
    {
      "name_zh": "分节名", "name_en": "Section Name",
      "items": [
        {
          "title_zh": "…", "title_en": "…",
          "summary_zh": "…", "summary_en": "…",
          "why_zh": "…", "why_en": "…",
          "source": "来源名称", "url": "链接，无则为 null",
          "importance": 1, "tags": ["tag"]
        }
      ]
    }
  ]
}"#
    .to_string();
    let extra = extra.trim();
    if !extra.is_empty() {
        prompt.push_str("\n\n附加编辑偏好：\n");
        prompt.push_str(extra);
    }
    prompt
}

fn build_user_prompt(date: &str, candidates: &[ranker::Candidate]) -> String {
    let mut out = format!(
        "日期：{date}\n以下是 {} 条候选情报，请按系统要求整理为日报 JSON：\n\n",
        candidates.len()
    );
    for (index, candidate) in candidates.iter().enumerate() {
        let item = &candidate.item;
        out.push_str(&format!(
            "[{}] {}\n    来源：{}（分类：{}）\n",
            index + 1,
            item.title,
            candidate.source_name,
            candidate.source_category,
        ));
        if let Some(url) = item.url.as_deref() {
            out.push_str(&format!("    链接：{url}\n"));
        }
        if let Some(at) = item.published_at {
            out.push_str(&format!(
                "    发布时间：{} UTC\n",
                at.format("%Y-%m-%d %H:%M")
            ));
        }
        let brief = item
            .summary
            .as_deref()
            .or(item.content.as_deref())
            .unwrap_or("");
        if !brief.is_empty() {
            out.push_str(&format!(
                "    摘要：{}\n",
                crate::news::normalize::truncate_chars(brief, 300)
            ));
        }
        if let Some(extra) = item.extra_json.as_deref() {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(extra) {
                if let Some(stars) = value.get("stars").and_then(|v| v.as_i64()) {
                    let today = value
                        .get("stars_today")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0);
                    out.push_str(&format!("    Stars：{stars}（今日 +{today}）\n"));
                }
                if let Some(lang) = value.get("language").and_then(|v| v.as_str()) {
                    out.push_str(&format!("    语言：{lang}\n"));
                }
            }
        }
        if let Some(keywords) = item.matched_keywords.as_deref() {
            out.push_str(&format!("    命中关键词：{keywords}\n"));
        }
        out.push_str(&format!("    筛选评分：{:.1}\n\n", candidate.score));
    }
    out
}

// ---------- Markdown 构建 ----------

/// 转义外部文本，防止注入原始 HTML（comrak 开启 unsafe）与破坏 Markdown 结构
fn md_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '[' => out.push_str("\\["),
            ']' => out.push_str("\\]"),
            '`' => out.push_str("\\`"),
            '\\' => out.push_str("\\\\"),
            '\n' | '\r' => out.push(' '),
            _ => out.push(c),
        }
    }
    out
}

/// URL 净化：仅允许 http(s)，并转义会破坏 Markdown 链接语法的字符
fn md_url(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return None;
    }
    Some(
        trimmed
            .replace(' ', "%20")
            .replace('(', "%28")
            .replace(')', "%29")
            .replace('<', "%3C")
            .replace('>', "%3E"),
    )
}

fn importance_stars(importance: i32) -> String {
    let filled = importance.clamp(1, 5) as usize;
    "★".repeat(filled) + &"☆".repeat(5 - filled)
}

pub fn build_markdown(doc: &llm::DigestDoc, lang: &str, raw_count: usize) -> String {
    let zh = lang != "en";
    let mut out = String::new();

    let selected = doc.item_count();
    if zh {
        out.push_str(&format!(
            "> 本文由 AI 情报管线自动生成：从过去 24 小时的 {raw_count} 条候选情报中精选 {selected} 条。所有链接均指向外部原文，内容请注意甄别。\n\n"
        ));
    } else {
        out.push_str(&format!(
            "> Auto-generated by the AI intelligence pipeline: {selected} items curated from {raw_count} candidates collected in the past 24 hours. All links point to external sources.\n\n"
        ));
    }

    let overview = if zh { &doc.summary_zh } else { &doc.summary_en };
    if !overview.trim().is_empty() {
        out.push_str(&md_escape(overview.trim()));
        out.push_str("\n\n");
    }

    for section in &doc.sections {
        let name = if zh {
            &section.name_zh
        } else {
            &section.name_en
        };
        let name = if name.trim().is_empty() {
            if zh {
                "情报"
            } else {
                "Intel"
            }
        } else {
            name.trim()
        };
        out.push_str(&format!("## {}\n\n", md_escape(name)));

        for item in &section.items {
            let title = if zh { &item.title_zh } else { &item.title_en };
            let title = md_escape(title.trim());
            out.push_str(&format!("<a id=\"{}\"></a>\n\n", item.anchor));
            match item.url.as_deref().and_then(md_url) {
                Some(url) => out.push_str(&format!("### [{title}]({url})\n\n")),
                None => out.push_str(&format!("### {title}\n\n")),
            }

            let summary = if zh {
                &item.summary_zh
            } else {
                &item.summary_en
            };
            if !summary.trim().is_empty() {
                out.push_str(&md_escape(summary.trim()));
                out.push_str("\n\n");
            }

            let why = if zh { &item.why_zh } else { &item.why_en };
            if !why.trim().is_empty() {
                let label = if zh {
                    "为什么值得关注"
                } else {
                    "Why it matters"
                };
                out.push_str(&format!("**{label}**：{}\n\n", md_escape(why.trim())));
            }

            let mut meta: Vec<String> = Vec::new();
            if !item.source.trim().is_empty() {
                let label = if zh { "来源" } else { "Source" };
                meta.push(format!("{label}：{}", md_escape(item.source.trim())));
            }
            let label = if zh { "重要度" } else { "Importance" };
            meta.push(format!("{label}：{}", importance_stars(item.importance)));
            if !item.tags.is_empty() {
                let label = if zh { "标签" } else { "Tags" };
                let tags = item
                    .tags
                    .iter()
                    .map(|t| format!("`{}`", t.replace('`', "")))
                    .collect::<Vec<_>>()
                    .join(" ");
                meta.push(format!("{label}：{tags}"));
            }
            out.push_str(&format!("{}\n\n", meta.join(" ／ ")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_doc() -> llm::DigestDoc {
        let raw = r#"{
            "summary_zh": "今日焦点是 <b>模型</b>",
            "summary_en": "Today's focus",
            "sections": [
                {"name_zh": "模型", "name_en": "Models", "items": [
                    {"title_zh": "新模型 [重磅]", "title_en": "New Model", "summary_zh": "摘要",
                     "summary_en": "Summary", "why_zh": "重要", "why_en": "Important",
                     "source": "HN", "url": "https://example.com/a(1)", "importance": 5,
                     "tags": ["llm"]}
                ]}
            ]
        }"#;
        llm::parse_digest_doc(raw).unwrap()
    }

    #[test]
    fn markdown_contains_anchor_and_escapes() {
        let doc = sample_doc();
        let md = build_markdown(&doc, "zh", 42);
        assert!(md.contains("<a id=\"intel-1\"></a>"));
        // 标题中的方括号被转义
        assert!(md.contains("\\[重磅\\]"));
        // URL 括号被百分号编码
        assert!(md.contains("https://example.com/a%281%29"));
        // 综述中的 HTML 被实体转义
        assert!(md.contains("&lt;b>模型"));
        assert!(md.contains("★★★★★"));
        assert!(md.contains("从过去 24 小时的 42 条候选情报中精选 1 条"));
    }

    #[test]
    fn markdown_english_variant() {
        let doc = sample_doc();
        let md = build_markdown(&doc, "en", 10);
        assert!(md.contains("## Models"));
        assert!(md.contains("Why it matters"));
        assert!(md.contains("New Model"));
    }

    #[test]
    fn md_url_rejects_non_http() {
        assert!(md_url("javascript:alert(1)").is_none());
        assert!(md_url("ftp://x").is_none());
        assert_eq!(md_url("https://a.com/b c").unwrap(), "https://a.com/b%20c");
    }

    #[test]
    fn stars_render() {
        assert_eq!(importance_stars(3), "★★★☆☆");
        assert_eq!(importance_stars(99), "★★★★★");
        assert_eq!(importance_stars(0), "★☆☆☆☆");
    }

    #[test]
    fn slug_and_title() {
        assert_eq!(digest_slug("2026-07-16"), "ai-daily-2026-07-16");
        assert_eq!(digest_title("zh", "2026-07-16"), "AI 前沿日报 | 2026-07-16");
        assert_eq!(
            digest_title("en", "2026-07-16"),
            "AI Frontier Daily | 2026-07-16"
        );
    }
}
