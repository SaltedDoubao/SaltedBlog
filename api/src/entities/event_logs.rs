use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "event_logs")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub occurred_at: DateTimeWithTimeZone,
    pub category: String,
    pub level: String,
    pub event_type: String,
    pub outcome: String,
    pub actor_user_id: Option<i32>,
    pub actor_name: Option<String>,
    pub source_ip: Option<String>,
    pub request_id: Option<String>,
    pub method: Option<String>,
    pub route: Option<String>,
    pub status_code: Option<i32>,
    pub duration_ms: Option<i64>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub summary: String,
    pub detail_json: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
