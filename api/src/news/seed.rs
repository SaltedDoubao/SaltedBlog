use chrono::Utc;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter,
    Set,
};

use crate::entities::{categories, news_sources, news_tasks, settings};

/// 日报文章使用的固定分类 slug
pub const DIGEST_CATEGORY_SLUG: &str = "ai-daily";

/// 种子化情报模块默认数据：日报分类 + 预置信源（仅表为空时）
pub async fn seed_defaults(db: &DatabaseConnection) -> anyhow::Result<()> {
    ensure_digest_category_id(db).await?;
    seed_sources(db).await?;
    seed_tasks_from_legacy_settings(db).await?;
    Ok(())
}

const TASKS_INITIALIZED_KEY: &str = "news_tasks_initialized";

/// 首次升级时把旧的全局调度参数转换为两条禁用任务。初始化标记避免删除后重建。
async fn seed_tasks_from_legacy_settings(db: &DatabaseConnection) -> anyhow::Result<()> {
    if settings::Entity::find_by_id(TASKS_INITIALIZED_KEY.to_string())
        .one(db)
        .await?
        .is_some()
    {
        return Ok(());
    }

    let rows = settings::Entity::find().all(db).await?;
    let get = |key: &str| {
        rows.iter()
            .find(|row| row.key == key)
            .map(|row| row.value.trim().to_string())
    };
    let interval = get("news_fetch_interval_hours")
        .and_then(|value| value.parse::<i32>().ok())
        .unwrap_or(2)
        .clamp(1, 24);
    let old_generation = get("news_digest_time").unwrap_or_else(|| "08:00".to_string());
    let (generation_time, publish_time) = migrated_digest_times(&old_generation);
    let auto_publish = get("news_digest_auto_publish").as_deref() != Some("false");
    let (publish_mode, publish_time) = migrated_publish_options(auto_publish, publish_time);
    let now = Utc::now();

    for task in [
        news_tasks::ActiveModel {
            name: Set("默认信息采集".to_string()),
            task_type: Set(news_tasks::TYPE_FETCH.to_string()),
            enabled: Set(false),
            start_time: Set(Some("00:00".to_string())),
            interval_hours: Set(Some(interval)),
            generation_time: Set(None),
            publish_time: Set(None),
            title_en: Set(None),
            publish_mode: Set(None),
            last_scheduled_at: Set(None),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        },
        news_tasks::ActiveModel {
            name: Set("AI 前沿日报".to_string()),
            task_type: Set(news_tasks::TYPE_DIGEST.to_string()),
            enabled: Set(false),
            start_time: Set(None),
            interval_hours: Set(None),
            generation_time: Set(Some(generation_time)),
            publish_time: Set(publish_time),
            title_en: Set(Some("AI Frontier Daily".to_string())),
            publish_mode: Set(Some(publish_mode)),
            last_scheduled_at: Set(None),
            created_at: Set(now.into()),
            updated_at: Set(now.into()),
            ..Default::default()
        },
    ] {
        task.insert(db).await?;
    }

    settings::ActiveModel {
        key: Set(TASKS_INITIALIZED_KEY.to_string()),
        value: Set("true".to_string()),
    }
    .insert(db)
    .await?;
    tracing::info!("initialized disabled news tasks from legacy settings");
    Ok(())
}

fn migrated_digest_times(raw: &str) -> (String, String) {
    let minutes = raw
        .split_once(':')
        .and_then(|(hour, minute)| Some((hour.parse::<u32>().ok()?, minute.parse::<u32>().ok()?)))
        .filter(|(hour, minute)| *hour < 24 && *minute < 60)
        .map(|(hour, minute)| hour * 60 + minute)
        .unwrap_or(8 * 60);
    let generation = if minutes >= 23 * 60 + 59 {
        22 * 60 + 59
    } else {
        minutes
    };
    let publish = (generation + 60).min(23 * 60 + 59);
    (
        format!("{:02}:{:02}", generation / 60, generation % 60),
        format!("{:02}:{:02}", publish / 60, publish % 60),
    )
}

fn migrated_publish_options(auto_publish: bool, publish_time: String) -> (String, Option<String>) {
    if auto_publish {
        (
            news_tasks::PUBLISH_MODE_SCHEDULED.to_string(),
            Some(publish_time),
        )
    } else {
        (news_tasks::PUBLISH_MODE_DRAFT.to_string(), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_digest_time_stays_within_day() {
        assert_eq!(
            migrated_digest_times("08:00"),
            ("08:00".into(), "09:00".into())
        );
        assert_eq!(
            migrated_digest_times("23:30"),
            ("23:30".into(), "23:59".into())
        );
        assert_eq!(
            migrated_digest_times("23:59"),
            ("22:59".into(), "23:59".into())
        );
        assert_eq!(
            migrated_digest_times("bad"),
            ("08:00".into(), "09:00".into())
        );
    }

    #[test]
    fn legacy_auto_publish_controls_task_mode() {
        assert_eq!(
            migrated_publish_options(true, "09:00".into()),
            ("scheduled".into(), Some("09:00".into()))
        );
        assert_eq!(
            migrated_publish_options(false, "09:00".into()),
            ("draft".into(), None)
        );
    }
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
            url: "https://www.v2ex.com/index.xml",
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
