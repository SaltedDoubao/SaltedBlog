use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "news_fetch_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub source_id: i32,
    pub status: String,
    pub fetched_count: i32,
    pub new_count: i32,
    pub duplicate_count: i32,
    pub excluded_count: i32,
    pub http_status: Option<i32>,
    pub error_message: Option<String>,
    pub started_at: DateTimeWithTimeZone,
    pub finished_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const STATUS_SUCCESS: &str = "success";
pub const STATUS_PARTIAL: &str = "partial";
pub const STATUS_FAILED: &str = "failed";
