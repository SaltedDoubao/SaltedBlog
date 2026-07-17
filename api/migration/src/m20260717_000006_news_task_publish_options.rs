use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(NewsTasks::Table)
                    .add_column(ColumnDef::new(NewsTasks::TitleEn).string_len(300))
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(NewsTasks::Table)
                    .add_column(ColumnDef::new(NewsTasks::PublishMode).string_len(16))
                    .to_owned(),
            )
            .await?;
        manager
            .get_connection()
            .execute_unprepared(
                "UPDATE news_tasks SET title_en = name, publish_mode = 'scheduled' WHERE task_type = 'digest'",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(NewsTasks::Table)
                    .drop_column(NewsTasks::PublishMode)
                    .to_owned(),
            )
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(NewsTasks::Table)
                    .drop_column(NewsTasks::TitleEn)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum NewsTasks {
    Table,
    TitleEn,
    PublishMode,
}
