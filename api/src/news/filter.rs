//! 关键词硬过滤：include 须至少命中一个（若配置），exclude 命中任一即拒绝
//! 匹配策略：含 CJK 的关键词用子串匹配，纯英文关键词用词界匹配（容忍单数 s 复数）

pub struct KeywordEvaluation {
    pub accepted: bool,
    pub reason: Option<&'static str>,
    pub matched_keywords: Vec<String>,
}

pub const REASON_EXCLUDED: &str = "excluded_keyword";
pub const REASON_INCLUDE_NOT_MATCHED: &str = "include_keyword_not_matched";

/// 逗号（中英文）/分号/换行分隔的关键词列表
pub fn parse_keywords(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or_default()
        .split([',', '，', ';', '；', '\n'])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn has_cjk(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(c,
            '\u{4E00}'..='\u{9FFF}'   // CJK 统一表意
            | '\u{3400}'..='\u{4DBF}' // 扩展 A
            | '\u{3040}'..='\u{30FF}' // 日文假名
            | '\u{AC00}'..='\u{D7AF}' // 韩文音节
        )
    })
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric()
}

/// 英文词界匹配（大小写不敏感，允许词尾多一个 s/S）
fn word_boundary_match(haystack_lower: &str, keyword_lower: &str) -> bool {
    if keyword_lower.is_empty() {
        return false;
    }
    let hay: Vec<char> = haystack_lower.chars().collect();
    let needle: Vec<char> = keyword_lower.chars().collect();
    if needle.len() > hay.len() {
        return false;
    }
    for start in 0..=(hay.len() - needle.len()) {
        if hay[start..start + needle.len()] != needle[..] {
            continue;
        }
        // 前界
        if start > 0 && is_word_char(hay[start - 1]) {
            continue;
        }
        // 后界（允许一个复数 s）
        let mut end = start + needle.len();
        if end < hay.len() && (hay[end] == 's') {
            end += 1;
        }
        if end < hay.len() && is_word_char(hay[end]) {
            continue;
        }
        return true;
    }
    false
}

/// 单个关键词是否命中文本
pub fn keyword_matches(text: &str, keyword: &str) -> bool {
    let text_lower = text.to_lowercase();
    let kw_lower = keyword.to_lowercase();
    if kw_lower.is_empty() {
        return false;
    }
    if has_cjk(&kw_lower) {
        text_lower.contains(&kw_lower)
    } else {
        word_boundary_match(&text_lower, &kw_lower)
    }
}

/// 评估条目：text 各部分拼接后统一匹配
pub fn evaluate(
    title: &str,
    summary: Option<&str>,
    content: Option<&str>,
    include_keywords: Option<&str>,
    exclude_keywords: Option<&str>,
) -> KeywordEvaluation {
    let text = format!(
        "{}\n{}\n{}",
        title,
        summary.unwrap_or_default(),
        content.unwrap_or_default()
    );

    for kw in parse_keywords(exclude_keywords) {
        if keyword_matches(&text, &kw) {
            return KeywordEvaluation {
                accepted: false,
                reason: Some(REASON_EXCLUDED),
                matched_keywords: Vec::new(),
            };
        }
    }

    let includes = parse_keywords(include_keywords);
    if includes.is_empty() {
        return KeywordEvaluation {
            accepted: true,
            reason: None,
            matched_keywords: Vec::new(),
        };
    }

    let matched: Vec<String> = includes
        .into_iter()
        .filter(|kw| keyword_matches(&text, kw))
        .collect();
    if matched.is_empty() {
        KeywordEvaluation {
            accepted: false,
            reason: Some(REASON_INCLUDE_NOT_MATCHED),
            matched_keywords: Vec::new(),
        }
    } else {
        KeywordEvaluation {
            accepted: true,
            reason: None,
            matched_keywords: matched,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_supports_mixed_separators() {
        let kws = parse_keywords(Some("AI, 大模型，rust；go\nml ; "));
        assert_eq!(kws, vec!["AI", "大模型", "rust", "go", "ml"]);
    }

    #[test]
    fn cjk_keyword_substring() {
        assert!(keyword_matches("最新大模型发布了", "大模型"));
        assert!(!keyword_matches("最新模型发布了", "大模型"));
    }

    #[test]
    fn english_word_boundary() {
        assert!(keyword_matches("New AI model released", "ai"));
        assert!(!keyword_matches("said something", "ai")); // said 中的 ai 不算词
        assert!(keyword_matches("multiple GPUs available", "gpu")); // 复数
        assert!(!keyword_matches("gpuX form", "gpu")); // 词尾还有字母
    }

    #[test]
    fn exclude_wins() {
        let eval = evaluate("AI crypto scam", None, None, Some("ai"), Some("crypto"));
        assert!(!eval.accepted);
        assert_eq!(eval.reason, Some(REASON_EXCLUDED));
    }

    #[test]
    fn include_required_when_configured() {
        let eval = evaluate("random topic", None, None, Some("ai,rust"), None);
        assert!(!eval.accepted);
        assert_eq!(eval.reason, Some(REASON_INCLUDE_NOT_MATCHED));

        let eval = evaluate("rust 1.99 released", None, None, Some("ai,rust"), None);
        assert!(eval.accepted);
        assert_eq!(eval.matched_keywords, vec!["rust"]);
    }

    #[test]
    fn no_keywords_accepts_all() {
        let eval = evaluate("anything", Some("goes"), None, None, None);
        assert!(eval.accepted);
        assert!(eval.reason.is_none());
    }

    #[test]
    fn summary_and_content_participate() {
        let eval = evaluate(
            "title",
            Some("mentions rust here"),
            None,
            Some("rust"),
            None,
        );
        assert!(eval.accepted);
        let eval = evaluate("title", None, Some("deep content rust"), Some("rust"), None);
        assert!(eval.accepted);
    }
}
