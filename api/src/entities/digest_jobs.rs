use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "digest_jobs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub digest_date: String,
    pub trigger: String,
    pub status: String,
    pub raw_count: i32,
    pub selected_count: i32,
    pub error_message: Option<String>,
    pub llm_model: Option<String>,
    pub result_json: Option<String>,
    pub post_id_zh: Option<i32>,
    pub post_id_en: Option<i32>,
    pub started_at: DateTimeWithTimeZone,
    pub finished_at: Option<DateTimeWithTimeZone>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const STATUS_RUNNING: &str = "running";
pub const STATUS_SUCCESS: &str = "success";
pub const STATUS_FAILED: &str = "failed";

pub const TRIGGER_AUTO: &str = "auto";
pub const TRIGGER_MANUAL: &str = "manual";
