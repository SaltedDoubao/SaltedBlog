use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "site_icons")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub upload_id: i32,
    pub is_active: bool,
    pub created_at: DateTimeWithTimeZone,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::uploads::Entity",
        from = "Column::UploadId",
        to = "super::uploads::Column::Id"
    )]
    Upload,
}

impl Related<super::uploads::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Upload.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
