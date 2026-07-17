use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "news_tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub task_type: String,
    pub enabled: bool,
    pub start_time: Option<String>,
    pub interval_hours: Option<i32>,
    pub generation_time: Option<String>,
    pub publish_time: Option<String>,
    pub publish_mode: Option<String>,
    pub last_scheduled_at: Option<DateTimeWithTimeZone>,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const TYPE_FETCH: &str = "fetch";
pub const TYPE_DIGEST: &str = "digest";
pub const PUBLISH_MODE_DRAFT: &str = "draft";
pub const PUBLISH_MODE_SCHEDULED: &str = "scheduled";
