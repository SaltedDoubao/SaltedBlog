use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{FixedOffset, Timelike, Utc};
use croner::Cron;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait};

use crate::backup;
use crate::entities::settings;
use crate::error::{ApiError, ApiResult};
use crate::logging::{self, NewEvent, CATEGORY_BACKUP, CATEGORY_SYSTEM};
use crate::state::AppState;

pub const ENABLED_KEY: &str = "backup_auto_enabled";
pub const CRON_KEY: &str = "backup_auto_cron";
const LAST_SLOT_KEY: &str = "backup_auto_last_scheduled_at";
pub const DEFAULT_CRON: &str = "0 0 * * *";

#[derive(Debug, Clone)]
pub struct AutoBackupSettings {
    pub enabled: bool,
    pub cron: String,
    pub last_scheduled_at: Option<String>,
}

pub async fn load_settings(db: &DatabaseConnection) -> Result<AutoBackupSettings, sea_orm::DbErr> {
    let rows = settings::Entity::find().all(db).await?;
    let get = |key: &str| {
        rows.iter()
            .find(|row| row.key == key)
            .map(|row| row.value.trim().to_string())
    };
    Ok(AutoBackupSettings {
        enabled: get(ENABLED_KEY).as_deref() == Some("true"),
        cron: get(CRON_KEY)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_CRON.into()),
        last_scheduled_at: get(LAST_SLOT_KEY).filter(|value| !value.is_empty()),
    })
}

pub fn normalize_cron(value: &str) -> ApiResult<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() || normalized.len() > 128 {
        return Err(ApiError::bad_request("Cron 表达式长度应为 1-128 个字符"));
    }
    let fields = normalized.split(' ').collect::<Vec<_>>();
    if fields.len() != 5 {
        return Err(ApiError::bad_request("自动备份仅支持 Cron 表达式"));
    }
    let minute = fields[0]
        .parse::<u8>()
        .map_err(|_| ApiError::bad_request("Cron 分钟字段必须是 0-59 的单个数字"))?;
    if minute > 59 {
        return Err(ApiError::bad_request("Cron 分钟字段必须是 0-59 的单个数字"));
    }
    Cron::from_str(&normalized).map_err(|_| ApiError::bad_request("Cron 表达式无效"))?;
    Ok(normalized)
}

async fn upsert_setting<C>(db: &C, key: &str, value: String) -> Result<(), sea_orm::DbErr>
where
    C: sea_orm::ConnectionTrait,
{
    match settings::Entity::find_by_id(key.to_string())
        .one(db)
        .await?
    {
        Some(row) => {
            let mut model: settings::ActiveModel = row.into();
            model.value = Set(value);
            model.update(db).await?;
        }
        None => {
            settings::ActiveModel {
                key: Set(key.to_string()),
                value: Set(value),
            }
            .insert(db)
            .await?;
        }
    }
    Ok(())
}

pub async fn save_settings(
    db: &DatabaseConnection,
    enabled: bool,
    cron: &str,
) -> ApiResult<String> {
    let cron = normalize_cron(cron)?;
    let txn = db.begin().await?;
    upsert_setting(&txn, ENABLED_KEY, enabled.to_string()).await?;
    upsert_setting(&txn, CRON_KEY, cron.clone()).await?;
    txn.commit().await?;
    Ok(cron)
}

async fn save_slot(db: &DatabaseConnection, slot: &str) -> Result<(), sea_orm::DbErr> {
    upsert_setting(db, LAST_SLOT_KEY, slot.to_string()).await
}

pub fn spawn(state: AppState) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        let pending = Arc::new(AtomicBool::new(false));
        let mut last_process_slot: Option<String> = None;
        let mut last_invalid_cron: Option<String> = None;
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        tracing::info!("automatic backup scheduler started");
        loop {
            interval.tick().await;
            let db = state.db();
            let config = match load_settings(&db).await {
                Ok(config) => config,
                Err(_) => {
                    tracing::error!(
                        error_code = "database_error",
                        "automatic backup settings load failed"
                    );
                    logging::record(
                        &state,
                        NewEvent {
                            category: CATEGORY_SYSTEM,
                            level: "error",
                            event_type: "backup.scheduler".into(),
                            outcome: "failure",
                            summary: "自动备份调度器读取配置失败".into(),
                            detail: Some(serde_json::json!({ "error_code": "database_error" })),
                            ..Default::default()
                        },
                    )
                    .await;
                    continue;
                }
            };
            if !config.enabled {
                last_invalid_cron = None;
                continue;
            }
            let cron_value = match normalize_cron(&config.cron) {
                Ok(value) => value,
                Err(_) => {
                    if last_invalid_cron.as_deref() != Some(&config.cron) {
                        logging::record(
                            &state,
                            NewEvent {
                                category: CATEGORY_BACKUP,
                                level: "error",
                                event_type: "backup.auto_config_invalid".into(),
                                outcome: "failure",
                                summary: "自动备份 Cron 配置无效，调度已停用".into(),
                                detail: Some(serde_json::json!({ "error_code": "invalid_cron" })),
                                ..Default::default()
                            },
                        )
                        .await;
                        last_invalid_cron = Some(config.cron);
                    }
                    continue;
                }
            };
            last_invalid_cron = None;
            let cron = match Cron::from_str(&cron_value) {
                Ok(cron) => cron,
                Err(_) => continue,
            };
            let offset = FixedOffset::east_opt(state.cfg.stats_tz_offset_hours * 3600)
                .unwrap_or_else(|| FixedOffset::east_opt(0).expect("UTC offset"));
            let now = Utc::now();
            let local = now
                .with_timezone(&offset)
                .with_second(0)
                .and_then(|value| value.with_nanosecond(0))
                .unwrap_or_else(|| now.with_timezone(&offset));
            if !cron.is_time_matching(&local).unwrap_or(false) {
                continue;
            }
            let slot = now
                .with_second(0)
                .and_then(|value| value.with_nanosecond(0))
                .unwrap_or(now)
                .to_rfc3339();
            if config.last_scheduled_at.as_deref() == Some(&slot)
                || last_process_slot.as_deref() == Some(&slot)
            {
                continue;
            }
            if save_slot(&db, &slot).await.is_err() {
                tracing::error!(
                    error_code = "database_error",
                    "automatic backup slot persistence failed"
                );
                continue;
            }
            last_process_slot = Some(slot.clone());
            if pending.swap(true, Ordering::AcqRel) {
                logging::record(
                    &state,
                    NewEvent {
                        category: CATEGORY_BACKUP,
                        level: "warn",
                        event_type: "backup.auto_skipped_running".into(),
                        outcome: "failure",
                        summary: "自动备份因已有待执行任务而跳过".into(),
                        detail: Some(serde_json::json!({ "scheduled_at": slot })),
                        ..Default::default()
                    },
                )
                .await;
                continue;
            }
            let state = state.clone();
            let pending = pending.clone();
            tokio::spawn(async move {
                let _guard = state.backup_lock.lock().await;
                let result = backup::create_backup(&state).await;
                match result {
                    Ok(name) => {
                        logging::record(
                            &state,
                            NewEvent {
                                category: CATEGORY_BACKUP,
                                level: "info",
                                event_type: "backup.auto_create".into(),
                                outcome: "success",
                                resource_type: Some("backup".into()),
                                resource_id: Some(name.clone()),
                                summary: "自动备份创建成功".into(),
                                detail: Some(serde_json::json!({
                                    "backup_name": name,
                                    "scheduled_at": slot,
                                    "trigger": "automatic",
                                })),
                                ..Default::default()
                            },
                        )
                        .await;
                    }
                    Err(error) => {
                        let failure = backup::public_failure(&error);
                        tracing::error!(error_code = failure.code, "automatic backup failed");
                        logging::record(
                            &state,
                            NewEvent {
                                category: CATEGORY_BACKUP,
                                level: "error",
                                event_type: "backup.auto_create".into(),
                                outcome: "failure",
                                summary: "自动备份创建失败".into(),
                                detail: Some(serde_json::json!({
                                    "scheduled_at": slot,
                                    "trigger": "automatic",
                                    "error_code": failure.code,
                                })),
                                ..Default::default()
                            },
                        )
                        .await;
                    }
                }
                pending.store(false, Ordering::Release);
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use migration::{Migrator, MigratorTrait};
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    #[test]
    fn cron_requires_five_fields_and_single_minute() {
        assert_eq!(normalize_cron("0 0 * * *").unwrap(), DEFAULT_CRON);
        assert_eq!(normalize_cron(" 15   */2 * * * ").unwrap(), "15 */2 * * *");
        assert!(normalize_cron("* * * * *").is_err());
        assert!(normalize_cron("0 0 0 * * *").is_err());
        assert!(normalize_cron("@daily").is_err());
        assert!(normalize_cron("60 0 * * *").is_err());
    }

    #[test]
    fn five_field_cron_matches_the_configured_local_minute() {
        let cron = Cron::from_str(DEFAULT_CRON).unwrap();
        let offset = FixedOffset::east_opt(8 * 3600).unwrap();
        let local = offset.with_ymd_and_hms(2026, 7, 21, 0, 0, 0).unwrap();
        assert!(cron.is_time_matching(&local).unwrap());
        assert!(!cron
            .is_time_matching(&offset.with_ymd_and_hms(2026, 7, 21, 0, 1, 0).unwrap())
            .unwrap());
    }

    #[tokio::test]
    async fn settings_default_and_atomic_validation() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, None).await.unwrap();
        let defaults = load_settings(&db).await.unwrap();
        assert!(!defaults.enabled);
        assert_eq!(defaults.cron, DEFAULT_CRON);

        save_settings(&db, true, "15 */2 * * *").await.unwrap();
        assert!(save_settings(&db, false, "* * * * *").await.is_err());
        let saved = load_settings(&db).await.unwrap();
        assert!(saved.enabled);
        assert_eq!(saved.cron, "15 */2 * * *");
    }

    #[tokio::test]
    async fn retry_migration_backfills_existing_task_types() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        Migrator::up(&db, Some(8)).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO news_tasks (name, task_type, enabled, created_at, updated_at) VALUES \
             ('fetch', 'fetch', 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP), \
             ('digest', 'digest', 0, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();
        Migrator::up(&db, None).await.unwrap();
        let rows = db
            .query_all(Statement::from_string(
                DatabaseBackend::Sqlite,
                "SELECT task_type, retry_count FROM news_tasks ORDER BY task_type".to_string(),
            ))
            .await
            .unwrap();
        let values = rows
            .into_iter()
            .map(|row| {
                (
                    row.try_get::<String>("", "task_type").unwrap(),
                    row.try_get::<i32>("", "retry_count").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(values, vec![("digest".into(), 2), ("fetch".into(), 0)]);
    }
}
