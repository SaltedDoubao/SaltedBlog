//! AI 情报聚合管线：信源采集 → 去重过滤 → 选稿 → LLM 中文日报 → 发布为博客文章
pub mod digest;
pub mod fetch;
pub mod fetcher;
pub mod filter;
pub mod llm;
pub mod normalize;
pub mod ranker;
pub mod scheduler;
pub mod seed;
pub mod tasks;

use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

use crate::entities::settings;

/// 情报模块运行配置（settings 表 news_ 前缀键，后台可改，每次使用时读取）
#[derive(Debug, Clone)]
pub struct NewsSettings {
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_extra_prompt: String,
    pub retention_days: i64,
    pub log_retention_days: i64,
}

impl Default for NewsSettings {
    fn default() -> Self {
        Self {
            llm_base_url: String::new(),
            llm_model: String::new(),
            llm_extra_prompt: String::new(),
            retention_days: 30,
            log_retention_days: 7,
        }
    }
}

pub async fn load_settings(db: &DatabaseConnection) -> Result<NewsSettings, sea_orm::DbErr> {
    let rows = settings::Entity::find()
        .filter(settings::Column::Key.starts_with("news_"))
        .all(db)
        .await?;
    let get = |key: &str| -> Option<String> {
        rows.iter()
            .find(|r| r.key == key)
            .map(|r| r.value.trim().to_string())
    };
    let mut out = NewsSettings::default();
    if let Some(v) = get("news_llm_base_url") {
        out.llm_base_url = v;
    }
    if let Some(v) = get("news_llm_model") {
        out.llm_model = v;
    }
    if let Some(v) = get("news_llm_extra_prompt") {
        out.llm_extra_prompt = v;
    }
    if let Some(v) = get("news_retention_days") {
        if let Ok(n) = v.parse::<i64>() {
            out.retention_days = n.clamp(1, 3650);
        }
    }
    if let Some(v) = get("news_log_retention_days") {
        if let Ok(n) = v.parse::<i64>() {
            out.log_retention_days = n.clamp(1, 3650);
        }
    }
    Ok(out)
}

/// 按站点时区偏移得到今天的日期串（YYYY-MM-DD）
pub fn local_date_string(tz_offset_hours: i32) -> String {
    let offset = chrono::FixedOffset::east_opt(tz_offset_hours * 3600)
        .unwrap_or_else(|| chrono::FixedOffset::east_opt(0).expect("utc offset"));
    chrono::Utc::now()
        .with_timezone(&offset)
        .format("%Y-%m-%d")
        .to_string()
}
