use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ---- users ----
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .col(
                        ColumnDef::new(Users::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Users::Username).string_len(64).not_null().unique_key())
                    .col(ColumnDef::new(Users::PasswordHash).string_len(256).not_null())
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- sessions ----
        manager
            .create_table(
                Table::create()
                    .table(Sessions::Table)
                    .col(ColumnDef::new(Sessions::Id).string_len(64).not_null().primary_key())
                    .col(ColumnDef::new(Sessions::UserId).integer().not_null())
                    .col(
                        ColumnDef::new(Sessions::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Sessions::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_sessions_expires")
                    .table(Sessions::Table)
                    .col(Sessions::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        // ---- categories ----
        manager
            .create_table(
                Table::create()
                    .table(Categories::Table)
                    .col(
                        ColumnDef::new(Categories::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Categories::Slug).string_len(128).not_null().unique_key())
                    .col(ColumnDef::new(Categories::NameZh).string_len(128).not_null())
                    .col(ColumnDef::new(Categories::NameEn).string_len(128).not_null())
                    .col(ColumnDef::new(Categories::SortOrder).integer().not_null().default(0))
                    .col(
                        ColumnDef::new(Categories::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- tags ----
        manager
            .create_table(
                Table::create()
                    .table(Tags::Table)
                    .col(
                        ColumnDef::new(Tags::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Tags::Slug).string_len(128).not_null().unique_key())
                    .col(ColumnDef::new(Tags::NameZh).string_len(128).not_null())
                    .col(ColumnDef::new(Tags::NameEn).string_len(128).not_null())
                    .col(
                        ColumnDef::new(Tags::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- series ----
        manager
            .create_table(
                Table::create()
                    .table(Series::Table)
                    .col(
                        ColumnDef::new(Series::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Series::Slug).string_len(128).not_null().unique_key())
                    .col(ColumnDef::new(Series::NameZh).string_len(128).not_null())
                    .col(ColumnDef::new(Series::NameEn).string_len(128).not_null())
                    .col(ColumnDef::new(Series::DescriptionZh).text())
                    .col(ColumnDef::new(Series::DescriptionEn).text())
                    .col(
                        ColumnDef::new(Series::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- posts ----
        manager
            .create_table(
                Table::create()
                    .table(Posts::Table)
                    .col(
                        ColumnDef::new(Posts::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Posts::GroupId).string_len(36).not_null())
                    .col(ColumnDef::new(Posts::Lang).string_len(8).not_null())
                    .col(ColumnDef::new(Posts::Slug).string_len(200).not_null())
                    .col(ColumnDef::new(Posts::Title).string_len(300).not_null())
                    .col(ColumnDef::new(Posts::Summary).text())
                    .col(ColumnDef::new(Posts::Cover).string_len(500))
                    .col(ColumnDef::new(Posts::ContentMd).text().not_null())
                    .col(ColumnDef::new(Posts::ContentHtml).text().not_null())
                    .col(ColumnDef::new(Posts::TocJson).text())
                    .col(ColumnDef::new(Posts::SearchText).text().not_null())
                    .col(ColumnDef::new(Posts::Status).string_len(16).not_null())
                    .col(ColumnDef::new(Posts::CategoryId).integer())
                    .col(ColumnDef::new(Posts::SeriesId).integer())
                    .col(ColumnDef::new(Posts::SeriesOrder).integer())
                    .col(ColumnDef::new(Posts::ViewCount).integer().not_null().default(0))
                    .col(
                        ColumnDef::new(Posts::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Posts::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Posts::PublishedAt).timestamp_with_time_zone())
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
        manager
            .create_index(
                Index::create()
                    .name("idx_posts_category")
                    .table(Posts::Table)
                    .col(Posts::CategoryId)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_posts_series")
                    .table(Posts::Table)
                    .col(Posts::SeriesId)
                    .to_owned(),
            )
            .await?;

        // ---- post_tags ----
        manager
            .create_table(
                Table::create()
                    .table(PostTags::Table)
                    .col(ColumnDef::new(PostTags::PostId).integer().not_null())
                    .col(ColumnDef::new(PostTags::TagId).integer().not_null())
                    .primary_key(
                        Index::create()
                            .col(PostTags::PostId)
                            .col(PostTags::TagId),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_post_tags_tag")
                    .table(PostTags::Table)
                    .col(PostTags::TagId)
                    .to_owned(),
            )
            .await?;

        // ---- friends ----
        manager
            .create_table(
                Table::create()
                    .table(Friends::Table)
                    .col(
                        ColumnDef::new(Friends::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Friends::Name).string_len(128).not_null())
                    .col(ColumnDef::new(Friends::Url).string_len(500).not_null())
                    .col(ColumnDef::new(Friends::Avatar).string_len(500))
                    .col(ColumnDef::new(Friends::Description).text())
                    .col(ColumnDef::new(Friends::SortOrder).integer().not_null().default(0))
                    .col(
                        ColumnDef::new(Friends::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // ---- settings (KV) ----
        manager
            .create_table(
                Table::create()
                    .table(Settings::Table)
                    .col(ColumnDef::new(Settings::Key).string_len(128).not_null().primary_key())
                    .col(ColumnDef::new(Settings::Value).text().not_null())
                    .to_owned(),
            )
            .await?;

        // ---- page_views ----
        manager
            .create_table(
                Table::create()
                    .table(PageViews::Table)
                    .col(
                        ColumnDef::new(PageViews::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PageViews::Path).string_len(500).not_null())
                    .col(ColumnDef::new(PageViews::Referrer).string_len(500))
                    .col(ColumnDef::new(PageViews::VisitorHash).string_len(64).not_null())
                    .col(ColumnDef::new(PageViews::Date).string_len(10).not_null())
                    .col(
                        ColumnDef::new(PageViews::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_page_views_date")
                    .table(PageViews::Table)
                    .col(PageViews::Date)
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_page_views_path")
                    .table(PageViews::Table)
                    .col(PageViews::Path)
                    .col(PageViews::Date)
                    .to_owned(),
            )
            .await?;

        // ---- uploads ----
        manager
            .create_table(
                Table::create()
                    .table(Uploads::Table)
                    .col(
                        ColumnDef::new(Uploads::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Uploads::Path).string_len(500).not_null())
                    .col(ColumnDef::new(Uploads::OriginalName).string_len(300).not_null())
                    .col(ColumnDef::new(Uploads::Mime).string_len(100).not_null())
                    .col(ColumnDef::new(Uploads::SizeBytes).big_integer().not_null())
                    .col(
                        ColumnDef::new(Uploads::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for table in [
            Table::drop().table(Uploads::Table).to_owned(),
            Table::drop().table(PageViews::Table).to_owned(),
            Table::drop().table(Settings::Table).to_owned(),
            Table::drop().table(Friends::Table).to_owned(),
            Table::drop().table(PostTags::Table).to_owned(),
            Table::drop().table(Posts::Table).to_owned(),
            Table::drop().table(Series::Table).to_owned(),
            Table::drop().table(Tags::Table).to_owned(),
            Table::drop().table(Categories::Table).to_owned(),
            Table::drop().table(Sessions::Table).to_owned(),
            Table::drop().table(Users::Table).to_owned(),
        ] {
            manager.drop_table(table).await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    Username,
    PasswordHash,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Sessions {
    Table,
    Id,
    UserId,
    ExpiresAt,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Categories {
    Table,
    Id,
    Slug,
    NameZh,
    NameEn,
    SortOrder,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Tags {
    Table,
    Id,
    Slug,
    NameZh,
    NameEn,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Series {
    Table,
    Id,
    Slug,
    NameZh,
    NameEn,
    DescriptionZh,
    DescriptionEn,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Posts {
    Table,
    Id,
    GroupId,
    Lang,
    Slug,
    Title,
    Summary,
    Cover,
    ContentMd,
    ContentHtml,
    TocJson,
    SearchText,
    Status,
    CategoryId,
    SeriesId,
    SeriesOrder,
    ViewCount,
    CreatedAt,
    UpdatedAt,
    PublishedAt,
}

#[derive(DeriveIden)]
enum PostTags {
    Table,
    PostId,
    TagId,
}

#[derive(DeriveIden)]
enum Friends {
    Table,
    Id,
    Name,
    Url,
    Avatar,
    Description,
    SortOrder,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Settings {
    Table,
    Key,
    Value,
}

#[derive(DeriveIden)]
enum PageViews {
    Table,
    Id,
    Path,
    Referrer,
    VisitorHash,
    Date,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Uploads {
    Table,
    Id,
    Path,
    OriginalName,
    Mime,
    SizeBytes,
    CreatedAt,
}
