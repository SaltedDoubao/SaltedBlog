use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "news_items")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub source_id: i32,
    pub title: String,
    pub url: Option<String>,
    pub summary: Option<String>,
    pub content: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<DateTimeWithTimeZone>,
    pub fetched_at: DateTimeWithTimeZone,
    pub extra_json: Option<String>,
    pub dedup_key: String,
    pub url_hash: Option<String>,
    pub title_hash: Option<String>,
    pub content_hash: Option<String>,
    pub status: String,
    pub filter_reason: Option<String>,
    pub matched_keywords: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_EXCLUDED: &str = "excluded";
pub const STATUS_PROCESSED: &str = "processed";
