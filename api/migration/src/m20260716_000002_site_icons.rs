use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SiteIcons::Table)
                    .col(
                        ColumnDef::new(SiteIcons::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SiteIcons::UploadId).integer().not_null())
                    .col(
                        ColumnDef::new(SiteIcons::IsActive)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(SiteIcons::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx_site_icons_upload_id")
                    .table(SiteIcons::Table)
                    .col(SiteIcons::UploadId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SiteIcons::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum SiteIcons {
    Table,
    Id,
    UploadId,
    IsActive,
    CreatedAt,
}
