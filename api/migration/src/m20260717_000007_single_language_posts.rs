use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 项目尚未上线：旧文章和双语日报均为测试数据，直接清空后切换模型。
        for statement in [
            "DELETE FROM post_tags",
            "DELETE FROM digest_jobs",
            "DELETE FROM posts",
        ] {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }

        for index in ["uq_posts_lang_slug", "idx_posts_list", "idx_posts_group"] {
            manager
                .drop_index(Index::drop().name(index).to_owned())
                .await?;
        }

        manager
            .alter_table(
                Table::alter()
                    .table(DigestJobs::Table)
                    .add_column(ColumnDef::new(DigestJobs::PostId).integer())
                    .to_owned(),
            )
            .await?;

        for column in [Posts::Lang, Posts::GroupId] {
            manager
                .alter_table(
                    Table::alter()
                        .table(Posts::Table)
                        .drop_column(column)
                        .to_owned(),
                )
                .await?;
        }
        for column in [DigestJobs::PostIdZh, DigestJobs::PostIdEn] {
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
            .alter_table(
                Table::alter()
                    .table(NewsTasks::Table)
                    .drop_column(NewsTasks::TitleEn)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("uq_posts_slug")
                    .table(Posts::Table)
                    .col(Posts::Slug)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_posts_list")
                    .table(Posts::Table)
                    .col(Posts::Status)
                    .col(Posts::PublishedAt)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(Index::drop().name("uq_posts_slug").to_owned())
            .await?;
        manager
            .drop_index(Index::drop().name("idx_posts_list").to_owned())
            .await?;

        for alteration in [
            Table::alter()
                .table(Posts::Table)
                .add_column(
                    ColumnDef::new(Posts::GroupId)
                        .string_len(36)
                        .not_null()
                        .default(""),
                )
                .to_owned(),
            Table::alter()
                .table(Posts::Table)
                .add_column(
                    ColumnDef::new(Posts::Lang)
                        .string_len(8)
                        .not_null()
                        .default("zh"),
                )
                .to_owned(),
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(ColumnDef::new(DigestJobs::PostIdZh).integer())
                .to_owned(),
            Table::alter()
                .table(DigestJobs::Table)
                .add_column(ColumnDef::new(DigestJobs::PostIdEn).integer())
                .to_owned(),
            Table::alter()
                .table(NewsTasks::Table)
                .add_column(ColumnDef::new(NewsTasks::TitleEn).string_len(300))
                .to_owned(),
        ] {
            manager.alter_table(alteration).await?;
        }

        manager
            .get_connection()
            .execute_unprepared("UPDATE digest_jobs SET post_id_zh = post_id")
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(DigestJobs::Table)
                    .drop_column(DigestJobs::PostId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("uq_posts_lang_slug")
                    .table(Posts::Table)
                    .col(Posts::Lang)
                    .col(Posts::Slug)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_posts_list")
                    .table(Posts::Table)
                    .col(Posts::Lang)
                    .col(Posts::Status)
                    .col(Posts::PublishedAt)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_posts_group")
                    .table(Posts::Table)
                    .col(Posts::GroupId)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Posts {
    Table,
    GroupId,
    Lang,
    Slug,
    Status,
    PublishedAt,
}

#[derive(DeriveIden)]
enum DigestJobs {
    Table,
    PostId,
    PostIdZh,
    PostIdEn,
}

#[derive(DeriveIden)]
enum NewsTasks {
    Table,
    TitleEn,
}
