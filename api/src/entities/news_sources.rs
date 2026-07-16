use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "news_sources")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub source_type: String,
    pub url: String,
    pub category: Option<String>,
    pub language: String,
    pub include_keywords: Option<String>,
    pub exclude_keywords: Option<String>,
    pub max_items: i32,
    pub enabled: bool,
    pub send_to_llm: bool,
    pub weight: f64,
    pub github_language: Option<String>,
    pub github_since: Option<String>,
    pub min_stars: Option<i32>,
    pub created_at: DateTimeWithTimeZone,
    pub updated_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

pub const TYPE_RSS: &str = "rss";
pub const TYPE_GITHUB_TRENDING: &str = "github_trending";
