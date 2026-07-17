//! 内置多任务调度：按任务采集、每日生成/发布日报、周期清理过期数据。
use chrono::{FixedOffset, Timelike, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, Set,
};

use crate::entities::{digest_jobs, news_fetch_logs, news_items, news_tasks};
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
            if let Err(error) = tick(&state).await {
                tracing::warn!("news scheduler tick error: {error}");
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
        maybe_fetch(&db, task, state.cfg.stats_tz_offset_hours).await?;
    }
    for task in enabled
        .iter()
        .filter(|task| task.task_type == news_tasks::TYPE_DIGEST)
    {
        maybe_generate(state, &db, task).await?;
        maybe_publish(state, &db, task).await?;
    }
    maybe_cleanup(&db, &settings).await?;
    Ok(())
}

async fn maybe_fetch(
    db: &DatabaseConnection,
    task: &news_tasks::Model,
    tz_offset_hours: i32,
) -> anyhow::Result<()> {
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
    model.update(db).await?;

    let db = db.clone();
    let task_id = task.id;
    let task_name = task.name.clone();
    tokio::spawn(async move {
        tracing::info!(task_id, task_name = %task_name, "scheduled news fetch started");
        let summaries = fetch::fetch_all(&db).await;
        let total_new: i32 = summaries.iter().map(|summary| summary.new).sum();
        tracing::info!(
            task_id,
            sources = summaries.len(),
            total_new,
            "scheduled news fetch finished"
        );
    });
    Ok(())
}

async fn maybe_generate(
    state: &AppState,
    db: &DatabaseConnection,
    task: &news_tasks::Model,
) -> anyhow::Result<()> {
    let today = local_date_string(state.cfg.stats_tz_offset_hours);
    let Some(generation_minutes) = task.generation_time.as_deref().and_then(tasks::parse_hhmm)
    else {
        return Ok(());
    };
    if !is_due_minute(
        local_minutes(state.cfg.stats_tz_offset_hours),
        generation_minutes,
    ) {
        return Ok(());
    }
    let existing = digest_jobs::Entity::find()
        .filter(
            sea_orm::Condition::all()
                .add(digest_jobs::Column::NewsTaskId.eq(task.id))
                .add(digest_jobs::Column::DigestDate.eq(&today)),
        )
        .one(db)
        .await?;
    if existing.is_some() {
        return Ok(());
    }

    let state = state.clone();
    let task = task.clone();
    let scheduled_date = today;
    tokio::spawn(async move {
        tracing::info!(task_id = task.id, task_name = %task.name, "scheduled digest generation started");
        match digest::generate(
            &state,
            &task,
            digest_jobs::TRIGGER_AUTO,
            false,
            Some(&scheduled_date),
        )
        .await
        {
            Ok(job) => {
                tracing::info!(job_id = job.id, status = %job.status, "scheduled digest generation finished")
            }
            Err(error) => tracing::warn!(
                task_id = task.id,
                "scheduled digest generation rejected: {error}"
            ),
        }
    });
    Ok(())
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
                .add(digest_jobs::Column::PublishedAt.is_null()),
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
    if !is_same_utc_minute(Utc::now().timestamp(), due.with_timezone(&Utc).timestamp()) {
        return Ok(());
    }
    match digest::publish_job(db, job.clone()).await {
        Ok(_) => tracing::info!(
            task_id = task.id,
            job_id = job.id,
            "scheduled digest published"
        ),
        Err(error) => tracing::warn!(
            task_id = task.id,
            job_id = job.id,
            "scheduled digest publish failed: {error}"
        ),
    }
    Ok(())
}

fn local_minutes(tz_offset_hours: i32) -> u32 {
    let offset = FixedOffset::east_opt(tz_offset_hours * 3600)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("utc offset"));
    let now = Utc::now().with_timezone(&offset);
    now.hour() * 60 + now.minute()
}

fn is_due_minute(current: u32, scheduled: u32) -> bool {
    current == scheduled
}

fn is_same_utc_minute(now_timestamp: i64, scheduled_timestamp: i64) -> bool {
    now_timestamp / 60 == scheduled_timestamp / 60
}

/// 每天从起始时间计算计划槽位，只在当前分钟精确命中时返回 true。
fn is_fetch_due_minute(current: u32, start: u32, interval_hours: u32) -> bool {
    if current < start || interval_hours == 0 {
        return false;
    }
    let step = interval_hours * 60;
    (current - start) % step == 0
}

/// 每小时清理一次过期条目与日志，不受任务开关影响。
async fn maybe_cleanup(db: &DatabaseConnection, settings: &NewsSettings) -> anyhow::Result<()> {
    static LAST_CLEANUP: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
        std::sync::OnceLock::new();
    let slot = LAST_CLEANUP.get_or_init(|| std::sync::Mutex::new(None));
    {
        let mut guard = slot.lock().expect("cleanup lock");
        if guard.is_some_and(|time| time.elapsed().as_secs() < 3600) {
            return Ok(());
        }
        *guard = Some(std::time::Instant::now());
    }

    let item_cutoff = Utc::now() - chrono::Duration::days(settings.retention_days);
    let removed_items = news_items::Entity::delete_many()
        .filter(news_items::Column::FetchedAt.lt(item_cutoff))
        .exec(db)
        .await?;
    let log_cutoff = Utc::now() - chrono::Duration::days(settings.log_retention_days);
    let removed_logs = news_fetch_logs::Entity::delete_many()
        .filter(news_fetch_logs::Column::StartedAt.lt(log_cutoff))
        .exec(db)
        .await?;
    if removed_items.rows_affected > 0 || removed_logs.rows_affected > 0 {
        tracing::info!(
            removed_items = removed_items.rows_affected,
            removed_logs = removed_logs.rows_affected,
            "news cleanup finished"
        );
    }
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
}
