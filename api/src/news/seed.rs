use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};

use crate::entities::{categories, news_sources};

/// 日报文章使用的固定分类 slug
pub const DIGEST_CATEGORY_SLUG: &str = "ai-daily";

/// 种子化情报模块默认数据：日报分类 + 预置信源（仅表为空时）
pub async fn seed_defaults(db: &DatabaseConnection) -> anyhow::Result<()> {
    ensure_digest_category_id(db).await?;
    seed_sources(db).await?;
    Ok(())
}

/// 确保「AI 日报」分类存在并返回其 id（被删除后会自动重建）
pub async fn ensure_digest_category_id(db: &DatabaseConnection) -> anyhow::Result<i32> {
    let existing = categories::Entity::find()
        .filter(categories::Column::Slug.eq(DIGEST_CATEGORY_SLUG))
        .one(db)
        .await?;
    if let Some(row) = existing {
        return Ok(row.id);
    }
    let row = categories::ActiveModel {
        slug: Set(DIGEST_CATEGORY_SLUG.to_string()),
        name_zh: Set("AI 日报".to_string()),
        name_en: Set("AI Daily".to_string()),
        sort_order: Set(0),
        created_at: Set(Utc::now().into()),
        ..Default::default()
    }
    .insert(db)
    .await?;
    tracing::info!("seeded digest category '{DIGEST_CATEGORY_SLUG}'");
    Ok(row.id)
}

struct SeedSource {
    name: &'static str,
    source_type: &'static str,
    url: &'static str,
    category: &'static str,
    language: &'static str,
    weight: f64,
    max_items: i32,
    github_since: Option<&'static str>,
    min_stars: Option<i32>,
}

async fn seed_sources(db: &DatabaseConnection) -> anyhow::Result<()> {
    let count = news_sources::Entity::find().count(db).await?;
    if count > 0 {
        return Ok(());
    }

    let seeds = [
        SeedSource {
            name: "Hacker News 头条",
            source_type: news_sources::TYPE_RSS,
            url: "https://hnrss.org/frontpage",
            category: "社区热榜",
            language: "en",
            weight: 1.2,
            max_items: 30,
            github_since: None,
            min_stars: None,
        },
        SeedSource {
            name: "GitHub Trending",
            source_type: news_sources::TYPE_GITHUB_TRENDING,
            url: "https://github.com/trending",
            category: "开源项目",
            language: "en",
            weight: 1.0,
            max_items: 25,
            github_since: Some("daily"),
            min_stars: Some(100),
        },
        SeedSource {
            name: "arXiv cs.AI",
            source_type: news_sources::TYPE_RSS,
            url: "https://rss.arxiv.org/rss/cs.AI",
            category: "论文前沿",
            language: "en",
            weight: 0.8,
            max_items: 30,
            github_since: None,
            min_stars: None,
        },
        SeedSource {
            name: "arXiv cs.CL",
            source_type: news_sources::TYPE_RSS,
            url: "https://rss.arxiv.org/rss/cs.CL",
            category: "论文前沿",
            language: "en",
            weight: 0.8,
            max_items: 30,
            github_since: None,
            min_stars: None,
        },
        SeedSource {
            name: "InfoQ 中文",
            source_type: news_sources::TYPE_RSS,
            url: "https://www.infoq.cn/feed",
            category: "技术资讯",
            language: "zh",
            weight: 1.0,
            max_items: 30,
            github_since: None,
            min_stars: None,
        },
        SeedSource {
            name: "V2EX 热门",
            source_type: news_sources::TYPE_RSS,
            url: "https://www.v2ex.com/feed/tab/hot.xml",
            category: "社区热榜",
            language: "zh",
            weight: 0.8,
            max_items: 30,
            github_since: None,
            min_stars: None,
        },
    ];

    let now = Utc::now();
    for seed in seeds {
        news_sources::ActiveModel {
            name: Set(seed.name.to_string()),
            source_type: Set(seed.source_type.to_string()),
            url: Set(seed.url.to_string()),
            category: Set(Some(seed.category.to_string())),
            language: Set(seed.language.to_string()),
            include_keywords: Set(None),
            exclude_keywords: Set(None),
            max_items: Set(seed.max_items),
            enabled: Set(true),
            send_to_llm: Set(true),
            weight: Set(seed.weight),
            github_language: Set(None),
            github_since: Set(seed.github_since.map(|s| s.to_string())),
            min_stars: Set(seed.min_stars),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        }
        .insert(db)
        .await?;
    }
    tracing::info!("seeded default news sources");
    Ok(())
}
