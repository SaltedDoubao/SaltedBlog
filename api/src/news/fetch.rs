//! 采集编排：抓取 → 规范化 → 指纹去重 → 关键词过滤 → 写入 news_items → 记录采集日志
use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, Set, SqlErr,
};

use crate::entities::{news_fetch_logs, news_items, news_sources};
use crate::logging::{self, EventContext, NewEvent, CATEGORY_JOB};
use crate::news::{fetcher, filter, normalize};
use crate::state::AppState;

#[derive(Debug, Clone, serde::Serialize)]
pub struct FetchSummary {
    pub source_id: i32,
    pub source_name: String,
    pub status: String,
    pub fetched: i32,
    pub new: i32,
    pub duplicate: i32,
    pub excluded: i32,
    pub error: Option<String>,
}

/// 采集所有 enabled 信源；单源失败不影响其他源
pub async fn fetch_all(db: &DatabaseConnection) -> Result<Vec<FetchSummary>, sea_orm::DbErr> {
    let sources = news_sources::Entity::find()
        .filter(news_sources::Column::Enabled.eq(true))
        .order_by_asc(news_sources::Column::Id)
        .all(db)
        .await?;
    let mut summaries = Vec::with_capacity(sources.len());
    for source in &sources {
        summaries.push(fetch_source(db, source).await);
    }
    Ok(summaries)
}

pub async fn record_summaries(
    state: &AppState,
    summaries: &[FetchSummary],
    trigger: &str,
    context: Option<&EventContext>,
) {
    for summary in summaries {
        let failed = summary.status != news_fetch_logs::STATUS_SUCCESS;
        let event = NewEvent {
            category: CATEGORY_JOB,
            level: if failed { "warn" } else { "info" },
            event_type: "news.fetch.source".into(),
            outcome: if failed { "failure" } else { "success" },
            resource_type: Some("news_source".into()),
            resource_id: Some(summary.source_id.to_string()),
            summary: if failed {
                "新闻信源采集未完全成功".into()
            } else {
                "新闻信源采集成功".into()
            },
            detail: Some(serde_json::json!({
                "trigger": trigger,
                "status": summary.status,
                "fetched": summary.fetched,
                "new": summary.new,
                "duplicate": summary.duplicate,
                "excluded": summary.excluded,
                "error_code": failed.then_some("source_fetch_failed"),
            })),
            ..Default::default()
        };
        let event = if let Some(value) = context {
            event.with_context(value)
        } else {
            event
        };
        logging::record(state, event).await;
    }
    let failed = summaries
        .iter()
        .filter(|summary| summary.status != news_fetch_logs::STATUS_SUCCESS)
        .count();
    let event = NewEvent {
        category: CATEGORY_JOB,
        level: if failed > 0 { "warn" } else { "info" },
        event_type: "news.fetch.batch".into(),
        outcome: if failed > 0 { "failure" } else { "success" },
        resource_type: Some("news_fetch".into()),
        summary: if failed > 0 {
            "新闻批量采集部分失败".into()
        } else {
            "新闻批量采集完成".into()
        },
        detail: Some(serde_json::json!({
            "trigger": trigger,
            "sources": summaries.len(),
            "failed_sources": failed,
        })),
        ..Default::default()
    };
    let event = if let Some(value) = context {
        event.with_context(value)
    } else {
        event
    };
    logging::record(state, event).await;
}

/// 采集单个信源并写日志
pub async fn fetch_source(db: &DatabaseConnection, source: &news_sources::Model) -> FetchSummary {
    let started_at = Utc::now();
    let outcome = fetcher::fetch_source_items(source).await;
    let fetched = outcome.items.len() as i32;

    let mut new_count = 0i32;
    let mut duplicate_count = 0i32;
    let mut excluded_count = 0i32;
    let mut error = outcome.error.clone();

    for item in &outcome.items {
        match ingest_item(db, source, item).await {
            Ok(IngestResult::New) => new_count += 1,
            Ok(IngestResult::NewExcluded) => {
                new_count += 1;
                excluded_count += 1;
            }
            Ok(IngestResult::Duplicate) => duplicate_count += 1,
            Err(_) => {
                tracing::warn!(
                    source_id = source.id,
                    error_code = "database_error",
                    "news ingest failed"
                );
                if error.is_none() {
                    error = Some("database write failed".into());
                }
            }
        }
    }

    // failed：有错误且无任何条目；partial：有条目但仍带错误；success：无错误
    let status = if error.is_some() && fetched == 0 {
        news_fetch_logs::STATUS_FAILED
    } else if error.is_some() {
        news_fetch_logs::STATUS_PARTIAL
    } else {
        news_fetch_logs::STATUS_SUCCESS
    };

    let log = news_fetch_logs::ActiveModel {
        source_id: Set(source.id),
        status: Set(status.to_string()),
        fetched_count: Set(fetched),
        new_count: Set(new_count),
        duplicate_count: Set(duplicate_count),
        excluded_count: Set(excluded_count),
        http_status: Set(outcome.http_status),
        error_message: Set(error.clone()),
        started_at: Set(started_at.into()),
        finished_at: Set(Some(Utc::now().into())),
        ..Default::default()
    };
    if log.insert(db).await.is_err() {
        tracing::error!(
            error_code = "database_error",
            "news fetch log insert failed"
        );
    }

    FetchSummary {
        source_id: source.id,
        source_name: source.name.clone(),
        status: status.to_string(),
        fetched,
        new: new_count,
        duplicate: duplicate_count,
        excluded: excluded_count,
        error,
    }
}

enum IngestResult {
    New,
    NewExcluded,
    Duplicate,
}

async fn ingest_item(
    db: &DatabaseConnection,
    source: &news_sources::Model,
    item: &fetcher::FetchedItem,
) -> Result<IngestResult, sea_orm::DbErr> {
    let fp = normalize::fingerprint(
        source.id,
        &item.title,
        item.url.as_deref(),
        item.content.as_deref(),
        item.summary.as_deref(),
    );
    let eval = filter::evaluate(
        &item.title,
        item.summary.as_deref(),
        item.content.as_deref(),
        source.include_keywords.as_deref(),
        source.exclude_keywords.as_deref(),
    );

    // 入库前查重：dedup_key（全局）→ url_hash（全局）→ title_hash（同源）→ content_hash（全局）
    if let Some(existing) = find_duplicate(db, source.id, &fp).await? {
        // 规则变更重激活：同源 excluded 且现规则接受 → 恢复 pending
        if existing.source_id == source.id
            && existing.status == news_items::STATUS_EXCLUDED
            && eval.accepted
        {
            let mut model: news_items::ActiveModel = existing.into();
            model.status = Set(news_items::STATUS_PENDING.to_string());
            model.filter_reason = Set(None);
            model.matched_keywords = Set(join_keywords(&eval.matched_keywords));
            model.update(db).await?;
        }
        return Ok(IngestResult::Duplicate);
    }

    let (status, reason) = if eval.accepted {
        (news_items::STATUS_PENDING, None)
    } else {
        (news_items::STATUS_EXCLUDED, eval.reason.map(String::from))
    };

    let model = news_items::ActiveModel {
        source_id: Set(source.id),
        title: Set(item.title.clone()),
        url: Set(fp.canonical_url.clone().or_else(|| item.url.clone())),
        summary: Set(item.summary.clone()),
        content: Set(item.content.clone()),
        author: Set(item.author.clone()),
        published_at: Set(item.published_at.map(Into::into)),
        fetched_at: Set(Utc::now().into()),
        extra_json: Set(item
            .extra
            .as_ref()
            .and_then(|v| serde_json::to_string(v).ok())),
        dedup_key: Set(fp.dedup_key),
        url_hash: Set(fp.url_hash),
        title_hash: Set(fp.title_hash),
        content_hash: Set(fp.content_hash),
        status: Set(status.to_string()),
        filter_reason: Set(reason),
        matched_keywords: Set(join_keywords(&eval.matched_keywords)),
        ..Default::default()
    };

    match model.insert(db).await {
        Ok(_) => Ok(if eval.accepted {
            IngestResult::New
        } else {
            IngestResult::NewExcluded
        }),
        // 并发兜底：唯一约束冲突计为 duplicate
        Err(e) if matches!(e.sql_err(), Some(SqlErr::UniqueConstraintViolation(_))) => {
            Ok(IngestResult::Duplicate)
        }
        Err(e) => Err(e),
    }
}

async fn find_duplicate(
    db: &DatabaseConnection,
    source_id: i32,
    fp: &normalize::Fingerprint,
) -> Result<Option<news_items::Model>, sea_orm::DbErr> {
    if let Some(found) = news_items::Entity::find()
        .filter(news_items::Column::DedupKey.eq(&fp.dedup_key))
        .one(db)
        .await?
    {
        return Ok(Some(found));
    }
    if let Some(hash) = &fp.url_hash {
        if let Some(found) = news_items::Entity::find()
            .filter(news_items::Column::UrlHash.eq(hash))
            .one(db)
            .await?
        {
            return Ok(Some(found));
        }
    }
    if let Some(hash) = &fp.title_hash {
        if let Some(found) = news_items::Entity::find()
            .filter(
                Condition::all()
                    .add(news_items::Column::SourceId.eq(source_id))
                    .add(news_items::Column::TitleHash.eq(hash)),
            )
            .one(db)
            .await?
        {
            return Ok(Some(found));
        }
    }
    if let Some(hash) = &fp.content_hash {
        if let Some(found) = news_items::Entity::find()
            .filter(news_items::Column::ContentHash.eq(hash))
            .one(db)
            .await?
        {
            return Ok(Some(found));
        }
    }
    Ok(None)
}

fn join_keywords(keywords: &[String]) -> Option<String> {
    if keywords.is_empty() {
        None
    } else {
        Some(keywords.join(","))
    }
}
