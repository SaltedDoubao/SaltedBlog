//! 选稿：多维评分 + 动态池大小 + 三阶段权重配额（最低覆盖 → 比例配额 → 全局填满）
use chrono::{DateTime, Utc};
use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};

use crate::entities::{news_items, news_sources};

/// 进入 LLM 的候选条目（附带评分与来源上下文）
#[derive(Debug, Clone)]
pub struct Candidate {
    pub item: news_items::Model,
    pub source_name: String,
    pub source_category: String,
    pub score: f64,
}

/// 时间窗内候选加载结果
pub struct CandidatePool {
    pub selected: Vec<Candidate>,
    /// 时间窗内 pending 总数（选稿前）
    pub raw_count: usize,
}

const WINDOW_HOURS: i64 = 24;

/// 动态候选池大小：M = min(100, max(50, 4n))
pub fn pool_size(active_source_count: usize) -> usize {
    (4 * active_source_count).clamp(50, 100)
}

/// 分类上限：C = min(ceil(0.4 * M), M)，M 为动态池大小
pub fn category_cap(pool: usize) -> usize {
    (((pool as f64) * 0.4).ceil() as usize).min(pool)
}

// ---------- 多维评分 ----------

/// source 维（上限 30）：clamp(weight,0,2)/2*30
fn source_score(weight: f64) -> f64 {
    weight.clamp(0.0, 2.0) / 2.0 * 30.0
}

/// freshness 维（上限 25）：按小时档衰减
fn freshness_score(published_at: Option<DateTime<Utc>>, fetched_at: DateTime<Utc>, now: DateTime<Utc>) -> f64 {
    let base = published_at.unwrap_or(fetched_at);
    let hours = (now - base).num_minutes() as f64 / 60.0;
    if hours <= 3.0 {
        25.0
    } else if hours <= 6.0 {
        20.0
    } else if hours <= 12.0 {
        15.0
    } else if hours <= 24.0 {
        10.0
    } else if hours <= 48.0 {
        5.0
    } else {
        0.0
    }
}

/// keyword 维（上限 20）：命中数 × 6
fn keyword_score(matched_keywords: Option<&str>) -> f64 {
    let count = matched_keywords
        .map(|s| s.split(',').filter(|k| !k.trim().is_empty()).count())
        .unwrap_or(0);
    ((count * 6) as f64).min(20.0)
}

/// popularity 维（上限 15）：GitHub stars 的 log10 分
fn popularity_score(extra_json: Option<&str>) -> f64 {
    let stars = extra_json
        .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
        .and_then(|v| v.get("stars").and_then(|s| s.as_i64()))
        .unwrap_or(0);
    if stars <= 0 {
        return 0.0;
    }
    (((stars + 1) as f64).log10() * 5.0).min(15.0)
}

/// quality 维（上限 10）：摘要/正文/作者/时间/链接完整度各 2 分
fn quality_score(item: &news_items::Model) -> f64 {
    let mut score = 0.0;
    if item.summary.as_deref().is_some_and(|s| s.chars().count() >= 20) {
        score += 2.0;
    }
    if item.content.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        score += 2.0;
    }
    if item.author.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        score += 2.0;
    }
    if item.published_at.is_some() {
        score += 2.0;
    }
    if item.url.as_deref().is_some_and(|s| !s.trim().is_empty()) {
        score += 2.0;
    }
    score
}

pub fn score_item(item: &news_items::Model, source_weight: f64, now: DateTime<Utc>) -> f64 {
    source_score(source_weight)
        + freshness_score(
            item.published_at.map(Into::into),
            item.fetched_at.into(),
            now,
        )
        + keyword_score(item.matched_keywords.as_deref())
        + popularity_score(item.extra_json.as_deref())
        + quality_score(item)
}

// ---------- 三阶段配额选择（纯函数，便于测试） ----------

#[derive(Debug, Clone)]
pub struct RankEntry {
    pub id: i32,
    pub source_id: i32,
    pub category: String,
    pub weight: f64,
    pub score: f64,
}

/// 选择过程的累计状态
struct SelectionState<'a> {
    selected: Vec<&'a RankEntry>,
    ids: std::collections::HashSet<i32>,
    per_source: std::collections::HashMap<i32, usize>,
    per_category: std::collections::HashMap<String, usize>,
}

impl<'a> SelectionState<'a> {
    fn new() -> Self {
        Self {
            selected: Vec::new(),
            ids: std::collections::HashSet::new(),
            per_source: std::collections::HashMap::new(),
            per_category: std::collections::HashMap::new(),
        }
    }

    fn contains(&self, id: i32) -> bool {
        self.ids.contains(&id)
    }

    fn source_count(&self, source_id: i32) -> usize {
        self.per_source.get(&source_id).copied().unwrap_or(0)
    }

    fn category_count(&self, category: &str) -> usize {
        self.per_category.get(category).copied().unwrap_or(0)
    }

    fn add(&mut self, entry: &'a RankEntry) {
        self.selected.push(entry);
        self.ids.insert(entry.id);
        *self.per_source.entry(entry.source_id).or_insert(0) += 1;
        *self.per_category.entry(entry.category.clone()).or_insert(0) += 1;
    }

    fn finish(mut self) -> Vec<i32> {
        self.selected.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
        self.selected.into_iter().map(|e| e.id).collect()
    }
}

/// 返回选中的条目 id（按分数降序）。
/// `active_source_count`：enabled 且 send_to_llm 的信源总数（决定池大小）。
pub fn select_quota(entries: &[RankEntry], active_source_count: usize) -> Vec<i32> {
    if entries.is_empty() {
        return Vec::new();
    }
    let pool = pool_size(active_source_count);
    let effective = pool.min(entries.len());
    let cat_cap = category_cap(pool);

    // 按源分组，组内按（分数降序，id 升序）排序
    let mut by_source: std::collections::BTreeMap<i32, Vec<&RankEntry>> =
        std::collections::BTreeMap::new();
    for entry in entries {
        by_source.entry(entry.source_id).or_default().push(entry);
    }
    for list in by_source.values_mut() {
        list.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        });
    }

    let source_weight = |sid: i32| -> f64 {
        by_source
            .get(&sid)
            .and_then(|l| l.first())
            .map(|e| e.weight.clamp(0.0, 2.0))
            .unwrap_or(0.0)
    };
    let active_with_candidates: Vec<i32> = by_source.keys().copied().collect();

    let mut state = SelectionState::new();

    // 池比活跃源还小：只给 weight 最高的前 effective 个源各 1 条
    if effective < active_with_candidates.len() {
        let mut ranked_sources = active_with_candidates.clone();
        ranked_sources.sort_by(|a, b| {
            source_weight(*b)
                .partial_cmp(&source_weight(*a))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(b))
        });
        for sid in ranked_sources.into_iter().take(effective) {
            if let Some(&top) = by_source.get(&sid).and_then(|l| l.first()) {
                state.add(top);
            }
        }
        return state.finish();
    }

    // 每源硬顶下限：ceil(0.25 * M)
    let min_hard_cap = ((pool as f64) * 0.25).ceil() as usize;

    // 阶段 1：最低覆盖 —— 每个有候选的源取其最高分 1 条（尊重分类上限）
    for sid in &active_with_candidates {
        let found = by_source[sid]
            .iter()
            .find(|e| !state.contains(e.id) && state.category_count(&e.category) < cat_cap)
            .copied();
        if let Some(entry) = found {
            state.add(entry);
        }
    }

    // 阶段 2：剩余名额按 weight 最大余数法分配目标 k_i
    let remaining = effective.saturating_sub(state.selected.len());
    let total_weight: f64 = active_with_candidates.iter().map(|s| source_weight(*s)).sum();
    let mut quota: std::collections::HashMap<i32, usize> = std::collections::HashMap::new();
    if remaining > 0 {
        let mut shares: Vec<(i32, f64)> = active_with_candidates
            .iter()
            .map(|sid| {
                let share = if total_weight > 0.0 {
                    remaining as f64 * source_weight(*sid) / total_weight
                } else {
                    remaining as f64 / active_with_candidates.len() as f64
                };
                (*sid, share)
            })
            .collect();
        let mut allocated: usize = shares.iter().map(|(_, s)| s.floor() as usize).sum();
        for (sid, share) in &shares {
            quota.insert(*sid, share.floor() as usize);
        }
        // 按小数部分降序补齐余数
        shares.sort_by(|a, b| {
            let fa = a.1 - a.1.floor();
            let fb = b.1 - b.1.floor();
            fb.partial_cmp(&fa)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let mut idx = 0;
        while allocated < remaining && !shares.is_empty() {
            let sid = shares[idx % shares.len()].0;
            *quota.entry(sid).or_insert(0) += 1;
            allocated += 1;
            idx += 1;
        }
    }
    // P_i = min(max(k_i_target, ceil(0.25*M)), M)，k_i_target 含阶段 1 的 1 条
    let hard_cap = |sid: i32| -> usize {
        let target = quota.get(&sid).copied().unwrap_or(0) + 1;
        target.max(min_hard_cap).min(pool)
    };
    for sid in &active_with_candidates {
        let take = quota.get(sid).copied().unwrap_or(0);
        if take == 0 {
            continue;
        }
        let mut taken = 0;
        for entry in by_source[sid].clone() {
            if taken >= take || state.selected.len() >= effective {
                break;
            }
            if state.contains(entry.id) {
                continue;
            }
            if state.source_count(*sid) >= hard_cap(*sid) {
                break;
            }
            if state.category_count(&entry.category) >= cat_cap {
                continue;
            }
            state.add(entry);
            taken += 1;
        }
    }

    // 阶段 3：按总分全局填满剩余槽位
    let mut all_sorted: Vec<&RankEntry> = entries.iter().collect();
    all_sorted.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    for entry in all_sorted {
        if state.selected.len() >= effective {
            break;
        }
        if state.contains(entry.id) {
            continue;
        }
        if state.source_count(entry.source_id) >= hard_cap(entry.source_id) {
            continue;
        }
        if state.category_count(&entry.category) >= cat_cap {
            continue;
        }
        state.add(entry);
    }

    state.finish()
}

// ---------- 从数据库加载并选稿 ----------

/// 加载 24h 时间窗内 pending 候选（信源须 enabled + send_to_llm），执行配额选稿
pub async fn load_candidates(db: &DatabaseConnection) -> Result<CandidatePool, sea_orm::DbErr> {
    let sources = news_sources::Entity::find()
        .filter(
            Condition::all()
                .add(news_sources::Column::Enabled.eq(true))
                .add(news_sources::Column::SendToLlm.eq(true)),
        )
        .all(db)
        .await?;
    if sources.is_empty() {
        return Ok(CandidatePool {
            selected: Vec::new(),
            raw_count: 0,
        });
    }
    let source_ids: Vec<i32> = sources.iter().map(|s| s.id).collect();
    let now = Utc::now();
    let cutoff = now - chrono::Duration::hours(WINDOW_HOURS);

    // 时间窗：优先 published_at，为空回退 fetched_at
    let window = Condition::any()
        .add(news_items::Column::PublishedAt.gte(cutoff))
        .add(
            Condition::all()
                .add(news_items::Column::PublishedAt.is_null())
                .add(news_items::Column::FetchedAt.gte(cutoff)),
        );
    let items = news_items::Entity::find()
        .filter(
            Condition::all()
                .add(news_items::Column::SourceId.is_in(source_ids))
                .add(news_items::Column::Status.eq(news_items::STATUS_PENDING))
                .add(window),
        )
        .order_by_desc(news_items::Column::FetchedAt)
        .all(db)
        .await?;
    let raw_count = items.len();

    let source_map: std::collections::HashMap<i32, &news_sources::Model> =
        sources.iter().map(|s| (s.id, s)).collect();

    let mut candidates: Vec<Candidate> = Vec::with_capacity(items.len());
    let mut entries: Vec<RankEntry> = Vec::with_capacity(items.len());
    for item in items {
        let Some(source) = source_map.get(&item.source_id) else {
            continue;
        };
        let score = score_item(&item, source.weight, now);
        let category = source
            .category
            .clone()
            .filter(|c| !c.trim().is_empty())
            .unwrap_or_else(|| "其他".to_string());
        entries.push(RankEntry {
            id: item.id,
            source_id: item.source_id,
            category: category.clone(),
            weight: source.weight,
            score,
        });
        candidates.push(Candidate {
            item,
            source_name: source.name.clone(),
            source_category: category,
            score,
        });
    }

    let selected_ids = select_quota(&entries, sources.len());
    let id_set: std::collections::HashSet<i32> = selected_ids.iter().copied().collect();
    let mut selected: Vec<Candidate> = candidates
        .into_iter()
        .filter(|c| id_set.contains(&c.item.id))
        .collect();
    selected.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.item.id.cmp(&b.item.id))
    });

    Ok(CandidatePool {
        selected,
        raw_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: i32, source_id: i32, category: &str, weight: f64, score: f64) -> RankEntry {
        RankEntry {
            id,
            source_id,
            category: category.to_string(),
            weight,
            score,
        }
    }

    #[test]
    fn pool_size_bounds() {
        assert_eq!(pool_size(1), 50);
        assert_eq!(pool_size(13), 52);
        assert_eq!(pool_size(30), 100);
    }

    #[test]
    fn empty_input() {
        assert!(select_quota(&[], 5).is_empty());
    }

    #[test]
    fn small_pool_takes_all() {
        // 候选远小于池大小：全部入选
        let entries = vec![
            entry(1, 1, "a", 1.0, 60.0),
            entry(2, 1, "a", 1.0, 50.0),
            entry(3, 2, "b", 1.0, 55.0),
        ];
        let picked = select_quota(&entries, 2);
        assert_eq!(picked.len(), 3);
        // 输出按分数降序
        assert_eq!(picked, vec![1, 3, 2]);
    }

    #[test]
    fn min_coverage_every_source_gets_one() {
        // 100 条同源高分 + 1 条他源低分：他源也要有 1 条
        let mut entries: Vec<RankEntry> = (0..100)
            .map(|i| entry(i, 1, "a", 2.0, 90.0 - i as f64 * 0.1))
            .collect();
        entries.push(entry(1000, 2, "b", 0.1, 1.0));
        let picked = select_quota(&entries, 2);
        assert!(picked.contains(&1000), "low-weight source must keep coverage");
    }

    #[test]
    fn category_cap_limits_single_category() {
        // 全部同分类：选中数不超过 C = min(ceil(0.4*M), M)
        let entries: Vec<RankEntry> = (0..60)
            .map(|i| entry(i, 1 + (i % 3), "same", 1.0, 80.0 - i as f64 * 0.1))
            .collect();
        let picked = select_quota(&entries, 3);
        let cap = category_cap(pool_size(3));
        assert!(picked.len() <= cap, "picked {} > cap {}", picked.len(), cap);
        assert_eq!(picked.len(), cap); // 候选充足时应打满分类上限
    }

    #[test]
    fn weight_shifts_allocation() {
        // 两源各 60 条候选，权重 2:0.5 → 高权重源占更多；
        // 双分类上限（各 20）导致总数低于 M=50 属预期行为
        let mut entries = Vec::new();
        for i in 0..60 {
            entries.push(entry(i, 1, "a", 2.0, 70.0 - i as f64 * 0.05));
        }
        for i in 60..120 {
            entries.push(entry(i, 2, "b", 0.5, 70.0 - (i - 60) as f64 * 0.05));
        }
        let picked = select_quota(&entries, 2);
        let cap = category_cap(pool_size(2));
        assert!(picked.len() <= 2 * cap);
        assert!(picked.len() >= 30, "expect a reasonably full pool, got {}", picked.len());
        let source1 = picked.iter().filter(|id| **id < 60).count();
        let source2 = picked.len() - source1;
        assert!(source1 > source2, "higher weight should win: {source1} vs {source2}");
        assert_eq!(source1, cap); // 高权重源打满分类上限
    }

    #[test]
    fn tiny_candidate_set_selects_all() {
        // 候选极少时不应被分类上限误伤（C 基于 M 而非候选数）
        let entries: Vec<RankEntry> = (0..3)
            .map(|i| entry(i, i, "c", 1.0 + i as f64 * 0.1, 50.0))
            .collect();
        let picked = select_quota(&entries, 3);
        assert_eq!(picked.len(), 3);
    }

    #[test]
    fn scores_components() {
        assert_eq!(source_score(2.0), 30.0);
        assert_eq!(source_score(1.0), 15.0);
        assert_eq!(source_score(-1.0), 0.0);
        assert_eq!(keyword_score(Some("a,b,c")), 18.0);
        assert_eq!(keyword_score(Some("a,b,c,d")), 20.0);
        assert_eq!(keyword_score(None), 0.0);
        // stars=999 → log10(1000)=3 → 15 分（触顶）
        let extra = r#"{"stars": 999}"#;
        assert_eq!(popularity_score(Some(extra)), 15.0);
        assert_eq!(popularity_score(Some(r#"{"stars": 9}"#)), 5.0);
        assert_eq!(popularity_score(None), 0.0);
    }
}
