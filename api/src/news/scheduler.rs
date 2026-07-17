//! 内置定时调度：按间隔采集、每日定点生成日报、周期清理过期数据。
//! 每分钟检查一次；news_enabled=false 时全部跳过（后台手动操作不受影响）。
use chrono::{FixedOffset, Utc};
use sea_orm::{
    ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};

use crate::entities::{digest_jobs, news_fetch_logs, news_items};
use crate::news::{digest, fetch, load_settings, local_date_string, NewsSettings};
use crate::state::AppState;

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        // 启动后稍等，避免与迁移/种子化竞争输出日志
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!("news scheduler started");
        loop {
            interval.tick().await;
            if let Err(e) = tick(&state).await {
                tracing::warn!("news scheduler tick error: {e}");
            }
        }
    });
}

async fn tick(state: &AppState) -> anyhow::Result<()> {
    let db = state.db();
    let settings = load_settings(&db).await?;
    if !settings.enabled {
        return Ok(());
    }

    maybe_fetch(&db, &settings).await?;
    maybe_digest(state, &db, &settings).await?;
    maybe_cleanup(&db, &settings).await?;
    Ok(())
}

/// 距最近一次采集超过配置间隔则采集（以采集日志为持久时钟，重启不丢状态）
async fn maybe_fetch(db: &DatabaseConnection, settings: &NewsSettings) -> anyhow::Result<()> {
    let last = news_fetch_logs::Entity::find()
        .order_by_desc(news_fetch_logs::Column::StartedAt)
        .limit(1)
        .one(db)
        .await?;
    let due = match last {
        None => true,
        Some(log) => {
            let elapsed = Utc::now() - log.started_at.with_timezone(&Utc);
            elapsed.num_minutes() >= settings.fetch_interval_hours * 60
        }
    };
    if !due {
        return Ok(());
    }
    tracing::info!("news scheduler: fetching all sources");
    let summaries = fetch::fetch_all(db).await;
    let total_new: i32 = summaries.iter().map(|s| s.new).sum();
    tracing::info!(
        "news scheduler: fetch finished, {} sources, {} new items",
        summaries.len(),
        total_new
    );
    Ok(())
}

/// 到达每日生成时间且当日尚无任务（任意状态）则生成；失败任务不自动重试
async fn maybe_digest(
    state: &AppState,
    db: &DatabaseConnection,
    settings: &NewsSettings,
) -> anyhow::Result<()> {
    let offset = FixedOffset::east_opt(state.cfg.stats_tz_offset_hours * 3600)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("utc offset"));
    let now_local = Utc::now().with_timezone(&offset);
    let minutes_of_day = now_local
        .format("%H")
        .to_string()
        .parse::<u32>()
        .unwrap_or(0)
        * 60
        + now_local
            .format("%M")
            .to_string()
            .parse::<u32>()
            .unwrap_or(0);
    let due_at = settings.digest_hour * 60 + settings.digest_minute;
    if minutes_of_day < due_at {
        return Ok(());
    }

    let today = local_date_string(state.cfg.stats_tz_offset_hours);
    let existing = digest_jobs::Entity::find()
        .filter(digest_jobs::Column::DigestDate.eq(&today))
        .count(db)
        .await?;
    if existing > 0 {
        return Ok(());
    }

    tracing::info!("news scheduler: generating digest for {today}");
    match digest::generate(state, digest_jobs::TRIGGER_AUTO, false).await {
        Ok(job) => tracing::info!("news scheduler: digest job {} -> {}", job.id, job.status),
        Err(e) => tracing::warn!("news scheduler: digest generation error: {e}"),
    }
    Ok(())
}

/// 每小时清理一次过期条目与日志
async fn maybe_cleanup(db: &DatabaseConnection, settings: &NewsSettings) -> anyhow::Result<()> {
    static LAST_CLEANUP: std::sync::OnceLock<std::sync::Mutex<Option<std::time::Instant>>> =
        std::sync::OnceLock::new();
    let slot = LAST_CLEANUP.get_or_init(|| std::sync::Mutex::new(None));
    {
        let mut guard = slot.lock().expect("cleanup lock");
        if guard.is_some_and(|t| t.elapsed().as_secs() < 3600) {
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
            "news cleanup: removed {} items, {} logs",
            removed_items.rows_affected,
            removed_logs.rows_affected
        );
    }
    Ok(())
}
