use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        macro_rules! add_column {
            ($table:ident, $column:expr) => {
                manager
                    .alter_table(
                        Table::alter()
                            .table($table::Table)
                            .add_column($column)
                            .to_owned(),
                    )
                    .await?;
            };
        }
        add_column!(Users, ColumnDef::new(Users::MfaSecretEnc).text());
        add_column!(
            Users,
            ColumnDef::new(Users::MfaEnabledAt).timestamp_with_time_zone()
        );
        add_column!(Users, ColumnDef::new(Users::LastTotpStep).big_integer());
        add_column!(
            Users,
            ColumnDef::new(Users::PasswordChangedAt).timestamp_with_time_zone()
        );
        add_column!(
            Sessions,
            ColumnDef::new(Sessions::CsrfHash)
                .string_len(64)
                .not_null()
                .default("")
        );
        add_column!(
            Sessions,
            ColumnDef::new(Sessions::LastSeenAt).timestamp_with_time_zone()
        );
        add_column!(
            Sessions,
            ColumnDef::new(Sessions::ElevatedUntil).timestamp_with_time_zone()
        );
        add_column!(Sessions, ColumnDef::new(Sessions::Ip).string_len(64));
        add_column!(
            Sessions,
            ColumnDef::new(Sessions::UserAgentHash).string_len(64)
        );

        manager
            .create_table(
                Table::create()
                    .table(PreauthTokens::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(PreauthTokens::Id)
                            .string_len(64)
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(PreauthTokens::UserId).integer().not_null())
                    .col(
                        ColumnDef::new(PreauthTokens::ExpiresAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(ColumnDef::new(PreauthTokens::MfaSecretEnc).text())
                    .col(
                        ColumnDef::new(PreauthTokens::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(PreauthTokens::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(PreauthTokens::Table, PreauthTokens::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_preauth_expires")
                    .table(PreauthTokens::Table)
                    .col(PreauthTokens::ExpiresAt)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(MfaRecoveryCodes::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(MfaRecoveryCodes::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(MfaRecoveryCodes::UserId)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(MfaRecoveryCodes::CodeHash)
                            .string_len(64)
                            .not_null(),
                    )
                    .col(ColumnDef::new(MfaRecoveryCodes::UsedAt).timestamp_with_time_zone())
                    .col(
                        ColumnDef::new(MfaRecoveryCodes::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(MfaRecoveryCodes::Table, MfaRecoveryCodes::UserId)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("idx_mfa_recovery_user")
                    .table(MfaRecoveryCodes::Table)
                    .col(MfaRecoveryCodes::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(EventLogs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(EventLogs::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(EventLogs::OccurredAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(EventLogs::Category)
                            .string_len(24)
                            .not_null(),
                    )
                    .col(ColumnDef::new(EventLogs::Level).string_len(16).not_null())
                    .col(
                        ColumnDef::new(EventLogs::EventType)
                            .string_len(96)
                            .not_null(),
                    )
                    .col(ColumnDef::new(EventLogs::Outcome).string_len(16).not_null())
                    .col(ColumnDef::new(EventLogs::ActorUserId).integer())
                    .col(ColumnDef::new(EventLogs::ActorName).string_len(64))
                    .col(ColumnDef::new(EventLogs::SourceIp).string_len(64))
                    .col(ColumnDef::new(EventLogs::RequestId).string_len(64))
                    .col(ColumnDef::new(EventLogs::Method).string_len(12))
                    .col(ColumnDef::new(EventLogs::Route).string_len(512))
                    .col(ColumnDef::new(EventLogs::StatusCode).integer())
                    .col(ColumnDef::new(EventLogs::DurationMs).big_integer())
                    .col(ColumnDef::new(EventLogs::ResourceType).string_len(64))
                    .col(ColumnDef::new(EventLogs::ResourceId).string_len(128))
                    .col(
                        ColumnDef::new(EventLogs::Summary)
                            .string_len(500)
                            .not_null(),
                    )
                    .col(ColumnDef::new(EventLogs::DetailJson).text())
                    .to_owned(),
            )
            .await?;
        for (name, columns) in [
            (
                "idx_event_logs_time",
                vec![EventLogs::OccurredAt, EventLogs::Id],
            ),
            (
                "idx_event_logs_category",
                vec![EventLogs::Category, EventLogs::OccurredAt],
            ),
            (
                "idx_event_logs_level",
                vec![EventLogs::Level, EventLogs::OccurredAt],
            ),
            (
                "idx_event_logs_request",
                vec![EventLogs::RequestId, EventLogs::OccurredAt],
            ),
        ] {
            let mut idx = Index::create();
            idx.name(name).table(EventLogs::Table);
            for column in columns {
                idx.col(column);
            }
            manager.create_index(idx.to_owned()).await?;
        }

        // 旧版会话保存的是明文令牌，安全迁移后全部撤销。
        manager
            .exec_stmt(Query::delete().from_table(Sessions::Table).to_owned())
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(EventLogs::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(MfaRecoveryCodes::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(PreauthTokens::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    MfaSecretEnc,
    MfaEnabledAt,
    LastTotpStep,
    PasswordChangedAt,
}
#[derive(DeriveIden)]
enum Sessions {
    Table,
    CsrfHash,
    LastSeenAt,
    ElevatedUntil,
    Ip,
    UserAgentHash,
}
#[derive(DeriveIden)]
enum PreauthTokens {
    Table,
    Id,
    UserId,
    ExpiresAt,
    MfaSecretEnc,
    Attempts,
    CreatedAt,
}
#[derive(DeriveIden)]
enum MfaRecoveryCodes {
    Table,
    Id,
    UserId,
    CodeHash,
    UsedAt,
    CreatedAt,
}
#[derive(DeriveIden)]
enum EventLogs {
    Table,
    Id,
    OccurredAt,
    Category,
    Level,
    EventType,
    Outcome,
    ActorUserId,
    ActorName,
    SourceIp,
    RequestId,
    Method,
    Route,
    StatusCode,
    DurationMs,
    ResourceType,
    ResourceId,
    Summary,
    DetailJson,
}
