mod auth;
mod backup;
mod config;
mod entities;
mod error;
mod news;
mod render;
mod routes;
mod state;

use std::net::SocketAddr;
use std::sync::Arc;

use migration::{Migrator, MigratorTrait};
use sea_orm::{ActiveModelTrait, Database, EntityTrait, PaginatorTrait, Set};
use tracing_subscriber::EnvFilter;

use crate::config::Config;
use crate::state::{AppStateInner, LoginLimiter};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // 把工作目录切到 .env 所在目录（仓库根），使 data/ 等相对路径
    // 无论从 api/ 还是根目录启动都保持一致；容器内无 .env 时不受影响
    if let Ok(env_path) = dotenvy::dotenv() {
        if let Some(dir) = env_path.parent() {
            let _ = std::env::set_current_dir(dir);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // reqwest 以 rustls-no-provider 构建，需在进程级安装 ring 作为默认 crypto provider
    let _ = rustls::crypto::ring::default_provider().install_default();

    let cfg = Config::from_env();

    // SQLite：确保数据库文件所在目录存在
    if let Some(path) = cfg
        .database_url
        .strip_prefix("sqlite://")
        .map(|rest| rest.split('?').next().unwrap_or(rest))
    {
        if let Some(parent) = std::path::Path::new(path).parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
    }
    std::fs::create_dir_all(&cfg.upload_dir)?;
    std::fs::create_dir_all(&cfg.backup_dir)?;

    tracing::info!("connecting to database...");
    let db = Database::connect(&cfg.database_url).await?;
    tracing::info!("running migrations...");
    Migrator::up(&db, None).await?;

    bootstrap_admin(&db, &cfg).await?;
    seed_settings(&db).await?;
    news::seed::seed_defaults(&db).await?;

    let state = Arc::new(AppStateInner::new(
        db,
        cfg,
        jieba_rs::Jieba::new(),
        LoginLimiter::new(),
    ));

    news::scheduler::spawn(state.clone());

    let app = routes::build_router(state.clone());
    let listener = tokio::net::TcpListener::bind(&state.cfg.bind_addr).await?;
    tracing::info!("salted-api listening on {}", state.cfg.bind_addr);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutting down");
}

/// users 表为空时创建初始管理员
async fn bootstrap_admin(db: &sea_orm::DatabaseConnection, cfg: &Config) -> anyhow::Result<()> {
    let count = entities::users::Entity::find().count(db).await?;
    if count > 0 {
        return Ok(());
    }
    if cfg.admin_password.is_empty() {
        tracing::warn!("users table is empty and ADMIN_PASSWORD is not set; admin login unavailable");
        return Ok(());
    }
    let hash = auth::hash_password(&cfg.admin_password)?;
    let user = entities::users::ActiveModel {
        username: Set(cfg.admin_username.clone()),
        password_hash: Set(hash),
        created_at: Set(chrono::Utc::now().into()),
        ..Default::default()
    };
    user.insert(db).await?;
    tracing::info!("created initial admin user '{}'", cfg.admin_username);
    Ok(())
}

/// 写入缺失的默认站点设置
async fn seed_settings(db: &sea_orm::DatabaseConnection) -> anyhow::Result<()> {
    let defaults: &[(&str, &str)] = &[
        ("site_title_zh", "SaltedBlog"),
        ("site_title_en", "SaltedBlog"),
        ("description_zh", "一个关于技术与生活的个人博客"),
        ("description_en", "A personal blog about tech and life"),
        ("home_eyebrow_zh", "个人博客 / 越过边界"),
        ("home_eyebrow_en", "PERSONAL BLOG / OVER THE FRONTIER"),
        ("home_news_title_zh", "最新情报"),
        ("home_news_title_en", "Latest Intelligence"),
        ("home_news_description_zh", "由 AI 情报管线每日聚合的前沿信号，点击进入完整日报。"),
        ("home_news_description_en", "Frontier signals aggregated daily by the AI intel pipeline. Open the full digest."),
        ("home_world_title_zh", "内容疆域"),
        ("home_world_title_en", "Content Frontier"),
        ("home_world_description_zh", "沿分类、系列与时间坐标进入博客的不同区域。"),
        ("home_world_description_en", "Enter the archive through categories, series, and chronological coordinates."),
        ("home_system_title_zh", "内容部署系统"),
        ("home_system_title_en", "Content Deployment System"),
        ("home_system_description_zh", "通过文章网络、分类矩阵与全文扫描定位所需内容。"),
        ("home_system_description_en", "Locate knowledge through the post network, taxonomy matrix, and full-text scanner."),
        ("home_operator_title_zh", "站点档案"),
        ("home_operator_title_en", "Site Personnel Archive"),
        ("home_operator_description_zh", "读取博主、站点与连接网络的授权档案。"),
        ("home_operator_description_en", "Read the authorized records of the author, the site, and its network."),
        ("home_protocol_title_zh", "跨越边界\n直至下一篇记录"),
        ("home_protocol_title_en", "Cross the Frontier\nToward the Next Record"),
        ("home_protocol_description_zh", "所有内容节点均已连接。选择一条路径，继续探索技术与生活的边界。"),
        ("home_protocol_description_en", "All content nodes are connected. Choose a path and continue beyond the frontier."),
        ("author", "Salted"),
        ("giscus_repo", ""),
        ("giscus_repo_id", ""),
        ("giscus_category", ""),
        ("giscus_category_id", ""),
        ("social_github", ""),
        ("social_email", ""),
        ("icp", ""),
        ("about_zh", "# 关于我\n\n这里还没有内容，请在后台「站点设置」中编辑。"),
        ("about_en", "# About\n\nNothing here yet. Edit it in admin settings."),
        // ---- AI 情报聚合（news_ 前缀不进公开接口）----
        ("news_enabled", "false"),
        ("news_fetch_interval_hours", "2"),
        ("news_digest_time", "08:00"),
        ("news_digest_auto_publish", "true"),
        ("news_llm_base_url", ""),
        ("news_llm_model", ""),
        ("news_llm_extra_prompt", ""),
        ("news_retention_days", "30"),
        ("news_log_retention_days", "7"),
    ];
    for (key, value) in defaults {
        let existing = entities::settings::Entity::find_by_id((*key).to_string())
            .one(db)
            .await?;
        if existing.is_none() {
            let model = entities::settings::ActiveModel {
                key: Set((*key).to_string()),
                value: Set((*value).to_string()),
            };
            model.insert(db).await?;
        }
    }
    Ok(())
}
