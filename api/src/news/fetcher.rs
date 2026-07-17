//! 采集器：按 source_type 路由到 RSS / GitHub Trending，输出统一的条目契约
use chrono::{DateTime, Utc};
use scraper::{Html, Selector};

use crate::entities::news_sources;
use crate::news::normalize::{strip_html, truncate_chars};

/// 统一条目契约（对应 ai-digest 文档中的 ItemDict）
#[derive(Debug, Clone, serde::Serialize)]
pub struct FetchedItem {
    pub title: String,
    pub url: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub author: Option<String>,
    pub extra: Option<serde_json::Value>,
}

pub struct FetchOutcome {
    pub items: Vec<FetchedItem>,
    pub error: Option<String>,
    pub http_status: Option<i32>,
}

impl FetchOutcome {
    fn fail(error: String, http_status: Option<i32>) -> Self {
        Self {
            items: Vec::new(),
            error: Some(error),
            http_status,
        }
    }
}

const TITLE_MAX: usize = 300;
const SUMMARY_MAX: usize = 2000;
const CONTENT_MAX: usize = 8000;

/// 工厂入口：按类型采集，未知类型显式报错
pub async fn fetch_source_items(source: &news_sources::Model) -> FetchOutcome {
    let max_items = source.max_items.clamp(1, 100) as usize;
    match source.source_type.as_str() {
        news_sources::TYPE_RSS => fetch_rss(&source.url, max_items).await,
        news_sources::TYPE_GITHUB_TRENDING => fetch_github_trending(source, max_items).await,
        other => FetchOutcome::fail(format!("unknown source_type '{other}'"), None),
    }
}

// ---------- RSS / Atom ----------

async fn fetch_rss(url: &str, max_items: usize) -> FetchOutcome {
    let response =
        match crate::outbound::get_bytes(url, 8 * 1024 * 1024, std::time::Duration::from_secs(30))
            .await
        {
            Ok(r) => r,
            Err(e) => return FetchOutcome::fail(format!("http error: {e}"), None),
        };
    let status = response.status.as_u16() as i32;
    if !response.status.is_success() {
        return FetchOutcome::fail(
            format!(
                "http status {status} after {} attempt(s)",
                response.attempts
            ),
            Some(status),
        );
    }
    if let Some(error) = empty_feed_error(&response.bytes, status) {
        return FetchOutcome::fail(error, Some(status));
    }
    let feed = match feed_rs::parser::parse(&response.bytes[..]) {
        Ok(f) => f,
        Err(e) => return FetchOutcome::fail(format!("feed parse error: {e}"), Some(status)),
    };

    let items = feed
        .entries
        .into_iter()
        .take(max_items)
        .filter_map(|entry| {
            let title = entry
                .title
                .map(|t| truncate_chars(strip_html(&t.content).trim(), TITLE_MAX))
                .unwrap_or_default();
            if title.is_empty() {
                return None;
            }
            let url = entry
                .links
                .first()
                .map(|l| l.href.clone())
                .filter(|s| !s.trim().is_empty());
            let summary = entry
                .summary
                .map(|s| truncate_chars(strip_html(&s.content).trim(), SUMMARY_MAX))
                .filter(|s| !s.is_empty());
            let content = entry
                .content
                .and_then(|c| c.body)
                .map(|b| truncate_chars(strip_html(&b).trim(), CONTENT_MAX))
                .filter(|s| !s.is_empty());
            let author = entry
                .authors
                .first()
                .map(|a| truncate_chars(a.name.trim(), 200))
                .filter(|s| !s.is_empty());
            Some(FetchedItem {
                title,
                url,
                summary,
                content,
                published_at: entry.published.or(entry.updated),
                author,
                extra: None,
            })
        })
        .collect();

    FetchOutcome {
        items,
        error: None,
        http_status: Some(status),
    }
}

fn empty_feed_error(bytes: &[u8], status: i32) -> Option<String> {
    bytes
        .is_empty()
        .then(|| format!("feed response body is empty (HTTP {status})"))
}

// ---------- GitHub Trending（HTML 解析） ----------

async fn fetch_github_trending(source: &news_sources::Model, max_items: usize) -> FetchOutcome {
    let language = source
        .github_language
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let since = source
        .github_since
        .as_deref()
        .map(str::trim)
        .filter(|s| matches!(*s, "daily" | "weekly" | "monthly"))
        .unwrap_or("daily");
    let url = match language {
        Some(lang) => format!("https://github.com/trending/{lang}?since={since}"),
        None => format!("https://github.com/trending?since={since}"),
    };

    let response =
        match crate::outbound::get_bytes(&url, 8 * 1024 * 1024, std::time::Duration::from_secs(30))
            .await
        {
            Ok(r) => r,
            Err(e) => return FetchOutcome::fail(format!("http error: {e}"), None),
        };
    let status = response.status.as_u16() as i32;
    if !response.status.is_success() {
        return FetchOutcome::fail(
            format!(
                "http status {status} after {} attempt(s)",
                response.attempts
            ),
            Some(status),
        );
    }
    let html = match String::from_utf8(response.bytes) {
        Ok(t) => t,
        Err(e) => return FetchOutcome::fail(format!("read body error: {e}"), Some(status)),
    };

    let items = parse_github_trending(&html, source.min_stars.unwrap_or(0), max_items);
    if items.is_empty() {
        return FetchOutcome {
            items,
            error: Some("no repos parsed from trending page (selectors may be outdated)".into()),
            http_status: Some(status),
        };
    }
    FetchOutcome {
        items,
        error: None,
        http_status: Some(status),
    }
}

/// 从 Trending HTML 提取仓库条目；解析失败的行跳过
fn parse_github_trending(html: &str, min_stars: i32, max_items: usize) -> Vec<FetchedItem> {
    let doc = Html::parse_document(html);
    let row_sel = Selector::parse("article.Box-row").expect("selector");
    let name_sel = Selector::parse("h2 a").expect("selector");
    let desc_sel = Selector::parse("p").expect("selector");
    let lang_sel = Selector::parse("span[itemprop=programmingLanguage]").expect("selector");
    let link_sel = Selector::parse("a").expect("selector");
    let today_sel = Selector::parse("span.d-inline-block.float-sm-right").expect("selector");

    let mut items = Vec::new();
    for row in doc.select(&row_sel) {
        let Some(name_el) = row.select(&name_sel).next() else {
            continue;
        };
        let Some(href) = name_el.value().attr("href") else {
            continue;
        };
        let repo_full_name = href.trim_matches('/').to_string();
        if repo_full_name.is_empty() {
            continue;
        }
        let repo_url = format!("https://github.com/{repo_full_name}");
        let description = row
            .select(&desc_sel)
            .next()
            .map(|p| collapse_text(&p.text().collect::<String>()))
            .filter(|s| !s.is_empty());
        let language = row
            .select(&lang_sel)
            .next()
            .map(|s| collapse_text(&s.text().collect::<String>()));
        // 总 star 数：href 以 /stargazers 结尾的链接文本
        let stars = row
            .select(&link_sel)
            .find(|a| {
                a.value()
                    .attr("href")
                    .is_some_and(|h| h.ends_with("/stargazers"))
            })
            .map(|a| parse_count(&a.text().collect::<String>()))
            .unwrap_or(0);
        let stars_today = row
            .select(&today_sel)
            .next()
            .map(|s| parse_count(&s.text().collect::<String>()))
            .unwrap_or(0);

        if min_stars > 0 && stars < min_stars as i64 {
            continue;
        }

        let owner = repo_full_name.split('/').next().unwrap_or("").to_string();
        items.push(FetchedItem {
            title: truncate_chars(&repo_full_name, TITLE_MAX),
            url: Some(repo_url),
            summary: description.map(|d| truncate_chars(&d, SUMMARY_MAX)),
            content: None,
            published_at: None,
            author: (!owner.is_empty()).then_some(owner),
            extra: Some(serde_json::json!({
                "stars": stars,
                "stars_today": stars_today,
                "language": language,
                "repo_full_name": repo_full_name,
            })),
        });
        if items.len() >= max_items {
            break;
        }
    }
    items
}

fn collapse_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 从 "12,345" / "1,234 stars today" 等文本中提取数字
fn parse_count(text: &str) -> i64 {
    let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_count_handles_commas_and_suffix() {
        assert_eq!(parse_count(" 12,345 "), 12345);
        assert_eq!(parse_count("678 stars today"), 678);
        assert_eq!(parse_count("no digits"), 0);
    }

    #[test]
    fn empty_feed_body_has_actionable_error() {
        assert_eq!(
            empty_feed_error(&[], 200).as_deref(),
            Some("feed response body is empty (HTTP 200)")
        );
        assert!(empty_feed_error(b"<rss />", 200).is_none());
    }

    #[test]
    fn parse_trending_html() {
        let html = r#"
        <html><body>
        <article class="Box-row">
          <h2 class="h3"><a href="/rust-lang/rust">rust-lang / rust</a></h2>
          <p class="col-9">Empowering everyone to build reliable software.</p>
          <span itemprop="programmingLanguage">Rust</span>
          <a href="/rust-lang/rust/stargazers">104,000</a>
          <span class="d-inline-block float-sm-right">120 stars today</span>
        </article>
        <article class="Box-row">
          <h2 class="h3"><a href="/tiny/repo">tiny / repo</a></h2>
          <a href="/tiny/repo/stargazers">50</a>
        </article>
        </body></html>"#;
        let items = parse_github_trending(html, 100, 10);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.title, "rust-lang/rust");
        assert_eq!(
            item.url.as_deref(),
            Some("https://github.com/rust-lang/rust")
        );
        assert_eq!(item.author.as_deref(), Some("rust-lang"));
        let extra = item.extra.as_ref().unwrap();
        assert_eq!(extra["stars"], 104000);
        assert_eq!(extra["stars_today"], 120);
        assert_eq!(extra["language"], "Rust");
    }

    #[test]
    fn parse_trending_respects_max_items() {
        let row = |name: &str| {
            format!(
                r#"<article class="Box-row"><h2><a href="/{name}">x</a></h2>
                <a href="/{name}/stargazers">500</a></article>"#
            )
        };
        let html = format!(
            "<html><body>{}{}{}</body></html>",
            row("a/1"),
            row("b/2"),
            row("c/3")
        );
        let items = parse_github_trending(&html, 0, 2);
        assert_eq!(items.len(), 2);
    }
}
