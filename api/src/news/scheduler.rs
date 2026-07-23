//! 内置多任务调度：按任务采集、每日生成/发布日报、周期清理过期数据。
use chrono::{FixedOffset, Timelike, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};

use crate::entities::{digest_jobs, news_fetch_logs, news_items, news_tasks};
use crate::logging::{self, NewEvent, CATEGORY_JOB, CATEGORY_SYSTEM};
use crate::news::{digest, fetch, load_settings, local_date_string, tasks, NewsSettings};
use crate::state::AppState;

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!("news scheduler started");
        loop {
            interval.tick().await;
            if tick(&state).await.is_err() {
                tracing::warn!(error_code = "database_error", "news scheduler tick failed");
                logging::record(
                    &state,
                    NewEvent {
                        category: CATEGORY_SYSTEM,
                        level: "error",
                        event_type: "news.scheduler.tick".into(),
                        outcome: "failure",
                        summary: "新闻调度器执行失败".into(),
                        detail: Some(serde_json::json!({ "error_code": "database_error" })),
                        ..Default::default()
                    },
                )
                .await;
            }
        }
    });
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let db = state.db();
    let settings = load_settings(&db).await?;
    let enabled = news_tasks::Entity::find()
        .filter(news_tasks::Column::Enabled.eq(true))
        .order_by_asc(news_tasks::Column::Id)
        .all(&db)
        .await?;

    for task in enabled
        .iter()
        .filter(|task| task.task_type == news_tasks::TYPE_FETCH)
    {
        if maybe_fetch(state, task, state.cfg.stats_tz_offset_hours)
            .await
            .is_err()
        {
            tracing::warn!(
                task_id = task.id,
                error_code = "schedule_error",
                "scheduled fetch task rejected"
            );
            record_schedule_failure(state, task, "fetch").await;
        }
    }
    for task in enabled
        .iter()
        .filter(|task| task.task_type == news_tasks::TYPE_DIGEST)
    {
        if maybe_generate(state, &db, task).await.is_err() {
            tracing::warn!(
                task_id = task.id,
                error_code = "schedule_error",
                "scheduled digest task rejected"
            );
            record_schedule_failure(state, task, "generate").await;
        }
        if maybe_publish(state, &db, task).await.is_err() {
            tracing::warn!(
                task_id = task.id,
                error_code = "schedule_error",
                "scheduled publish task rejected"
            );
            record_schedule_failure(state, task, "publish").await;
        }
    }
    if maybe_cleanup(state, &settings).await.is_err() {
        tracing::error!(
            error_code = "database_error",
            "news retention cleanup failed"
        );
        logging::record(
            state,
            NewEvent {
                category: CATEGORY_SYSTEM,
                level: "error",
                event_type: "news.retention_cleanup".into(),
                outcome: "failure",
                summary: "新闻保留策略清理失败".into(),
                detail: Some(serde_json::json!({ "error_code": "database_error" })),
                ..Default::default()
            },
        )
        .await;
    }
    Ok(())
}

async fn record_schedule_failure(state: &AppState, task: &news_tasks::Model, phase: &str) {
    logging::record(
        state,
        NewEvent {
            category: CATEGORY_JOB,
            level: "error",
            event_type: "news.task.schedule".into(),
            outcome: "failure",
            resource_type: Some("news_task".into()),
            resource_id: Some(task.id.to_string()),
            summary: "新闻任务调度失败".into(),
            detail: Some(serde_json::json!({ "phase": phase, "error_code": "database_error" })),
            ..Default::default()
        },
    )
    .await;
}

async fn maybe_fetch(
    state: &AppState,
    task: &news_tasks::Model,
    tz_offset_hours: i32,
) -> anyhow::Result<()> {
    let db = state.db();
    let Some(start) = task.start_time.as_deref().and_then(tasks::parse_hhmm) else {
        return Ok(());
    };
    let interval = task.interval_hours.unwrap_or(0);
    if !(1..=24).contains(&interval) {
        return Ok(());
    }
    let offset = FixedOffset::east_opt(tz_offset_hours * 3600)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("utc offset"));
    let now = Utc::now();
    let local = now.with_timezone(&offset);
    let current = local.hour() * 60 + local.minute();
    if !is_fetch_due_minute(current, start, interval as u32) {
        return Ok(());
    }
    let slot_minutes = current;
    let date = local.format("%Y-%m-%d").to_string();
    let slot = tasks::scheduled_utc(
        &date,
        &format!("{:02}:{:02}", slot_minutes / 60, slot_minutes % 60),
        tz_offset_hours,
    )
    .ok_or_else(|| anyhow::anyhow!("无法计算采集任务时点"))?;
    if task
        .last_scheduled_at
        .is_some_and(|last| last.with_timezone(&Utc) >= slot)
    {
        return Ok(());
    }

    let mut model: news_tasks::ActiveModel = task.clone().into();
    model.last_scheduled_at = Set(Some(slot.into()));
    model.updated_at = Set(now.into());
    model.update(&db).await?;

    let state = state.clone();
    let task_id = task.id;
    let task_name = task.name.clone();
    tokio::spawn(async move {
        tracing::info!(task_id, task_name = %task_name, "scheduled news fetch started");
        match fetch::fetch_all(&state.db()).await {
            Ok(summaries) => {
                let total_new: i32 = summaries.iter().map(|summary| summary.new).sum();
                fetch::record_summaries(&state, &summaries, "automatic", None).await;
                tracing::info!(
                    task_id,
                    sources = summaries.len(),
                    total_new,
                    "scheduled news fetch finished"
                );
            }
            Err(_) => {
                tracing::error!(
                    task_id,
                    error_code = "database_error",
                    "scheduled news fetch failed"
                );
                logging::record(
                    &state,
                    NewEvent {
                        category: CATEGORY_JOB,
                        level: "error",
                        event_type: "news.fetch.batch".into(),
                        outcome: "failure",
                        resource_type: Some("news_task".into()),
                        resource_id: Some(task_id.to_string()),
                        summary: "新闻批量采集无法启动".into(),
                        detail: Some(serde_json::json!({
                            "trigger": "automatic",
                            "error_code": "database_error",
                        })),
                        ..Default::default()
                    },
                )
                .await;
            }
        }
    });
    Ok(())
}

async fn maybe_generate(
    state: &AppState,
    db: &DatabaseConnection,
    task: &news_tasks::Model,
) -> anyhow::Result<()> {
    let today = local_date_string(state.cfg.stats_tz_offset_hours);
    let Some(generation_time) = task.generation_time.as_deref() else {
        return Ok(());
    };
    let Some(due) = tasks::scheduled_utc(&today, generation_time, state.cfg.stats_tz_offset_hours)
    else {
        return Ok(());
    };
    let now = Utc::now();
    if now < due {
        return Ok(());
    }
    let jobs = digest_jobs::Entity::find()
        .filter(
            sea_orm::Condition::all()
                .add(digest_jobs::Column::NewsTaskId.eq(task.id))
                .add(digest_jobs::Column::DigestDate.eq(&today)),
        )
        .order_by_desc(digest_jobs::Column::Id)
        .all(db)
        .await?;
    if jobs.iter().any(|job| {
        matches!(
            job.status.as_str(),
            digest_jobs::STATUS_RUNNING | digest_jobs::STATUS_SUCCESS
        )
    }) {
        return Ok(());
    }

    let automatic_failures = jobs
        .iter()
        .filter(|job| {
            job.status == digest_jobs::STATUS_FAILED
                && matches!(
                    job.trigger.as_str(),
                    digest_jobs::TRIGGER_AUTO | digest_jobs::TRIGGER_AUTO_RETRY
                )
        })
        .collect::<Vec<_>>();
    let Some((attempt, max_attempts)) =
        next_automatic_attempt(automatic_failures.len(), task.retry_count)
    else {
        return Ok(());
    };
    if let Some(latest) = automatic_failures.first() {
        let retry_from = latest
            .finished_at
            .unwrap_or(latest.started_at)
            .with_timezone(&Utc)
            + chrono::Duration::minutes(1);
        if now < retry_from {
            return Ok(());
        }
    }
    let trigger = if attempt == 1 {
        digest_jobs::TRIGGER_AUTO
    } else {
        digest_jobs::TRIGGER_AUTO_RETRY
    };

    let state = state.clone();
    let task = task.clone();
    let scheduled_date = today;
    tokio::spawn(async move {
        tracing::info!(task_id = task.id, task_name = %task.name, attempt, max_attempts, "scheduled digest generation started");
        match digest::generate(&state, &task, trigger, false, Some(&scheduled_date)).await {
            Ok(job) if job.status == digest_jobs::STATUS_SUCCESS => {
                tracing::info!(
                    job_id = job.id,
                    attempt,
                    "scheduled digest generation finished"
                );
                logging::record(
                    &state,
                    NewEvent {
                        category: CATEGORY_JOB,
                        level: "info",
                        event_type: "news.digest.generate".into(),
                        outcome: "success",
                        resource_type: Some("digest_job".into()),
                        resource_id: Some(job.id.to_string()),
                        summary: "日报自动生成成功".into(),
                        detail: Some(serde_json::json!({
                            "task_id": task.id,
                            "date": scheduled_date,
                            "attempt": attempt,
                            "max_attempts": max_attempts,
                            "trigger": trigger,
                            "raw_count": job.raw_count,
                            "selected_count": job.selected_count,
                            "post_id": job.post_id,
                        })),
                        ..Default::default()
                    },
                )
                .await;
            }
            Ok(job) => {
                tracing::warn!(
                    job_id = job.id,
                    attempt,
                    "scheduled digest generation failed"
                );
                logging::record(
                    &state,
                    NewEvent {
                        category: CATEGORY_JOB,
                        level: "error",
                        event_type: "news.digest.generate".into(),
                        outcome: "failure",
                        resource_type: Some("digest_job".into()),
                        resource_id: Some(job.id.to_string()),
                        summary: "日报自动生成失败".into(),
                        detail: Some(serde_json::json!({
                            "task_id": task.id,
                            "date": scheduled_date,
                            "attempt": attempt,
                            "max_attempts": max_attempts,
                            "trigger": trigger,
                            "error_code": "generation_failed",
                        })),
                        ..Default::default()
                    },
                )
                .await;
                if attempt == max_attempts {
                    logging::record(
                        &state,
                        NewEvent {
                            category: CATEGORY_JOB,
                            level: "error",
                            event_type: "news.digest.retry_exhausted".into(),
                            outcome: "failure",
                            resource_type: Some("news_task".into()),
                            resource_id: Some(task.id.to_string()),
                            summary: "日报自动生成重试次数已耗尽".into(),
                            detail: Some(serde_json::json!({
                                "date": scheduled_date,
                                "attempts": max_attempts,
                            })),
                            ..Default::default()
                        },
                    )
                    .await;
                }
            }
            Err(_) => {
                tracing::warn!(
                    task_id = task.id,
                    attempt,
                    error_code = "orchestration_error",
                    "scheduled digest generation rejected"
                );
                logging::record(
                    &state,
                    NewEvent {
                        category: CATEGORY_SYSTEM,
                        level: "error",
                        event_type: "news.digest.generate".into(),
                        outcome: "failure",
                        resource_type: Some("news_task".into()),
                        resource_id: Some(task.id.to_string()),
                        summary: "日报自动生成无法启动".into(),
                        detail: Some(serde_json::json!({
                            "date": scheduled_date,
                            "attempt": attempt,
                            "max_attempts": max_attempts,
                            "error_code": "orchestration_error",
                        })),
                        ..Default::default()
                    },
                )
                .await;
            }
        }
    });
    Ok(())
}

fn next_automatic_attempt(failed_attempts: usize, retry_count: i32) -> Option<(usize, usize)> {
    let max_attempts = 1 + retry_count.clamp(0, 5) as usize;
    (failed_attempts < max_attempts).then_some((failed_attempts + 1, max_attempts))
}

async fn maybe_publish(
    state: &AppState,
    db: &DatabaseConnection,
    task: &news_tasks::Model,
) -> anyhow::Result<()> {
    if task.publish_mode.as_deref() != Some(news_tasks::PUBLISH_MODE_SCHEDULED) {
        return Ok(());
    }
    let today = local_date_string(state.cfg.stats_tz_offset_hours);
    let job = digest_jobs::Entity::find()
        .filter(
            sea_orm::Condition::all()
                .add(digest_jobs::Column::NewsTaskId.eq(task.id))
                .add(digest_jobs::Column::DigestDate.eq(&today))
                .add(digest_jobs::Column::Status.eq(digest_jobs::STATUS_SUCCESS))
                .add(digest_jobs::Column::PublishedAt.is_null())
                .add(digest_jobs::Column::PublishError.is_null()),
        )
        .order_by_desc(digest_jobs::Column::Id)
        .one(db)
        .await?;
    let Some(job) = job else {
        return Ok(());
    };
    let Some(due) = job.scheduled_publish_at else {
        return Ok(());
    };
    if Utc::now() < due.with_timezone(&Utc) {
        return Ok(());
    }
    match digest::publish_job(db, job.clone()).await {
        Ok(_) => {
            tracing::info!(
                task_id = task.id,
                job_id = job.id,
                "scheduled digest published"
            );
            logging::record(
                state,
                NewEvent {
                    category: CATEGORY_JOB,
                    level: "info",
                    event_type: "news.digest.publish".into(),
                    outcome: "success",
                    resource_type: Some("digest_job".into()),
                    resource_id: Some(job.id.to_string()),
                    summary: "日报定时发布成功".into(),
                    detail: Some(serde_json::json!({ "task_id": task.id, "post_id": job.post_id })),
                    ..Default::default()
                },
            )
            .await;
        }
        Err(_) => {
            tracing::warn!(
                task_id = task.id,
                job_id = job.id,
                error_code = "publish_failed",
                "scheduled digest publish failed"
            );
            logging::record(
                state,
                NewEvent {
                    category: CATEGORY_JOB,
                    level: "error",
                    event_type: "news.digest.publish".into(),
                    outcome: "failure",
                    resource_type: Some("digest_job".into()),
                    resource_id: Some(job.id.to_string()),
                    summary: "日报定时发布失败".into(),
                    detail: Some(serde_json::json!({
                        "task_id": task.id,
                        "post_id": job.post_id,
                        "error_code": "publish_failed",
                    })),
                    ..Default::default()
                },
            )
            .await;
        }
    }
    Ok(())
}

#[cfg(test)]
fn is_due_minute(current: u32, scheduled: u32) -> bool {
    current == scheduled
}

#[cfg(test)]
fn is_same_utc_minute(now_timestamp: i64, scheduled_timestamp: i64) -> bool {
    now_timestamp / 60 == scheduled_timestamp / 60
}

/// 每天从起始时间计算计划槽位，只在当前分钟精确命中时返回 true。
fn is_fetch_due_minute(current: u32, start: u32, interval_hours: u32) -> bool {
    if current < start || interval_hours == 0 {
        return false;
    }
    let step = interval_hours * 60;
    (current - start).is_multiple_of(step)
}

/// 每小时清理一次过期条目与日志，不受任务开关影响。
async fn maybe_cleanup(state: &AppState, settings: &NewsSettings) -> anyhow::Result<()> {
    static LAST_CLEANUP: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
        std::sync::OnceLock::new();
    let slot = LAST_CLEANUP.get_or_init(|| std::sync::Mutex::new(None));
    {
        let guard = slot.lock().expect("cleanup lock");
        if guard.is_some_and(|time| time.elapsed().as_secs() < 3600) {
            return Ok(());
        }
    }

    let db = state.db();
    let item_cutoff = Utc::now() - chrono::Duration::days(settings.retention_days);
    let removed_items = news_items::Entity::delete_many()
        .filter(news_items::Column::FetchedAt.lt(item_cutoff))
        .exec(&db)
        .await?;
    let log_cutoff = Utc::now() - chrono::Duration::days(settings.log_retention_days);
    let removed_logs = news_fetch_logs::Entity::delete_many()
        .filter(news_fetch_logs::Column::StartedAt.lt(log_cutoff))
        .exec(&db)
        .await?;
    *slot.lock().expect("cleanup lock") = Some(std::time::Instant::now());
    if removed_items.rows_affected > 0 || removed_logs.rows_affected > 0 {
        tracing::info!(
            removed_items = removed_items.rows_affected,
            removed_logs = removed_logs.rows_affected,
            "news cleanup finished"
        );
    }
    logging::record(
        state,
        NewEvent {
            category: CATEGORY_JOB,
            level: "info",
            event_type: "news.retention_cleanup".into(),
            outcome: "success",
            summary: "新闻保留策略清理完成".into(),
            detail: Some(serde_json::json!({
                "removed_items": removed_items.rows_affected,
                "removed_logs": removed_logs.rows_affected,
            })),
            ..Default::default()
        },
    )
    .await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_slots_require_exact_minute() {
        assert!(!is_fetch_due_minute(7 * 60, 8 * 60, 2));
        assert!(is_fetch_due_minute(8 * 60, 8 * 60, 2));
        assert!(is_fetch_due_minute(12 * 60, 8 * 60, 2));
        assert!(!is_fetch_due_minute(12 * 60 + 1, 8 * 60, 2));
        assert!(!is_fetch_due_minute(13 * 60 + 59, 8 * 60, 2));
    }

    #[test]
    fn fetch_slots_restart_each_day() {
        assert!(!is_fetch_due_minute(60, 8 * 60, 5));
        assert!(is_fetch_due_minute(18 * 60, 8 * 60, 5));
    }

    #[test]
    fn digest_events_require_exact_minute() {
        assert!(is_due_minute(8 * 60, 8 * 60));
        assert!(!is_due_minute(8 * 60 + 1, 8 * 60));
        assert!(is_same_utc_minute(120, 179));
        assert!(!is_same_utc_minute(180, 179));
    }

    #[test]
    fn automatic_retry_budget_counts_extra_attempts() {
        assert_eq!(next_automatic_attempt(0, 2), Some((1, 3)));
        assert_eq!(next_automatic_attempt(1, 2), Some((2, 3)));
        assert_eq!(next_automatic_attempt(2, 2), Some((3, 3)));
        assert_eq!(next_automatic_attempt(3, 2), None);
        assert_eq!(next_automatic_attempt(1, 0), None);
        assert_eq!(next_automatic_attempt(5, 99), Some((6, 6)));
        assert_eq!(next_automatic_attempt(6, 99), None);
    }
}
