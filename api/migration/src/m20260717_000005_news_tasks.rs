use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(NewsTasks::Table)
                    .col(
                        ColumnDef::new(NewsTasks::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(NewsTasks::Name).string_len(128).not_null())
                    .col(
                        ColumnDef::new(NewsTasks::TaskType)
                            .string_len(16)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NewsTasks::Enabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(NewsTasks::StartTime).string_len(5))
                    .col(ColumnDef::new(NewsTasks::IntervalHours).integer())
                    .col(ColumnDef::new(NewsTasks::GenerationTime).string_len(5))
                    .col(ColumnDef::new(NewsTasks::PublishTime).string_len(5))
                    .col(ColumnDef::new(NewsTasks::LastScheduledAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(NewsTasks::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(NewsTasks::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_news_tasks_enabled_type")
                    .table(NewsTasks::Table)
                    .col(NewsTasks::Enabled)
                    .col(NewsTasks::TaskType)
                    .to_owned(),
            )
            .await?;

        for column in [
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(ColumnDef::new(DigestJobs::NewsTaskId).integer())
                .to_owned(),
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(ColumnDef::new(DigestJobs::TaskName).string_len(128))
                .to_owned(),
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(
                    ColumnDef::new(DigestJobs::ScheduledPublishAt).timestamp_with_time_zone(),
                )
                .to_owned(),
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(ColumnDef::new(DigestJobs::PublishedAt).timestamp_with_time_zone())
                .to_owned(),
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(ColumnDef::new(DigestJobs::PublishError).text())
                .to_owned(),
        ] {
            manager.alter_table(column).await?;
        }
        manager
            .create_index(
                Index::create()
                    .name("idx_digest_jobs_task_date")
                    .table(DigestJobs::Table)
                    .col(DigestJobs::NewsTaskId)
                    .col(DigestJobs::DigestDate)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(Index::drop().name("idx_digest_jobs_task_date").to_owned())
            .await?;
        for column in [
            DigestJobs::PublishError,
            DigestJobs::PublishedAt,
            DigestJobs::ScheduledPublishAt,
            DigestJobs::TaskName,
            DigestJobs::NewsTaskId,
        ] {
            manager
                .alter_table(
                    Table::alter()
                        .table(DigestJobs::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        manager
            .drop_table(Table::drop().table(NewsTasks::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum NewsTasks {
    Table,
    Id,
    Name,
    TaskType,
    Enabled,
    StartTime,
    IntervalHours,
    GenerationTime,
    PublishTime,
    LastScheduledAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum DigestJobs {
    Table,
    DigestDate,
    NewsTaskId,
    TaskName,
    ScheduledPublishAt,
    PublishedAt,
    PublishError,
}
