//! 内容规范化与去重指纹：canonical URL、标题归一、三类 hash 与 dedup_key
use sha2::{Digest, Sha256};

/// 常见跟踪参数（完整匹配，utm_ 为前缀匹配）
const TRACKING_PARAMS: &[&str] = &[
    "gclid",
    "fbclid",
    "yclid",
    "igshid",
    "spm",
    "ref_src",
    "mc_cid",
    "mc_eid",
    "share_token",
    "utm",
];

fn is_tracking_param(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with("utm_") || TRACKING_PARAMS.contains(&lower.as_str())
}

/// URL 规范化：小写 host、去 fragment、去跟踪参数、查询参数排序
pub fn canonical_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut parsed = url::Url::parse(trimmed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    parsed.set_fragment(None);
    let mut pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(name, _)| !is_tracking_param(name))
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    pairs.sort();
    if pairs.is_empty() {
        parsed.set_query(None);
    } else {
        let query = pairs
            .iter()
            .map(|(name, value)| {
                if value.is_empty() {
                    name.clone()
                } else {
                    format!("{name}={value}")
                }
            })
            .collect::<Vec<_>>()
            .join("&");
        parsed.set_query(Some(&query));
    }
    // host 小写（url crate 已对 host 做小写处理），去掉末尾多余斜杠（根路径除外）
    let mut out = parsed.to_string();
    if out.ends_with('/') && parsed.path() != "/" {
        out.pop();
    }
    Some(out)
}

/// 标题归一：小写 + 空白折叠
pub fn normalized_title(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn sha256_hex(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

/// 内容 hash：规范化标题 + 正文（优先）或摘要；正文过短时不生成，避免短文本误杀
pub fn content_hash(title: &str, content: Option<&str>, summary: Option<&str>) -> Option<String> {
    let body = content
        .filter(|s| !s.trim().is_empty())
        .or(summary.filter(|s| !s.trim().is_empty()))?;
    let body = body.trim();
    if body.chars().count() < 40 {
        return None;
    }
    Some(sha256_hex(&format!(
        "{}\n{}",
        normalized_title(title),
        body
    )))
}

pub struct Fingerprint {
    pub dedup_key: String,
    pub url_hash: Option<String>,
    pub title_hash: Option<String>,
    pub content_hash: Option<String>,
    pub canonical_url: Option<String>,
}

/// 按优先级生成 dedup_key：URL → 同源标题 → 内容 → 空兜底
pub fn fingerprint(
    source_id: i32,
    title: &str,
    url: Option<&str>,
    content: Option<&str>,
    summary: Option<&str>,
) -> Fingerprint {
    let canonical = url.and_then(canonical_url);
    let url_hash = canonical.as_deref().map(sha256_hex);
    let norm_title = normalized_title(title);
    let title_hash = (!norm_title.is_empty()).then(|| sha256_hex(&norm_title));
    let c_hash = content_hash(title, content, summary);

    let dedup_key = if let Some(hash) = &url_hash {
        format!("url:{hash}")
    } else if let Some(hash) = &title_hash {
        format!("source_title:{source_id}:{hash}")
    } else if let Some(hash) = &c_hash {
        format!("content:{hash}")
    } else {
        format!("empty:{source_id}")
    };

    Fingerprint {
        dedup_key,
        url_hash,
        title_hash,
        content_hash: c_hash,
        canonical_url: canonical,
    }
}

/// 去除 HTML 标签并解码常见实体，用于 RSS 摘要清洗
pub fn strip_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_tag = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '<' => in_tag = true,
            '>' => {
                if in_tag {
                    in_tag = false;
                    out.push(' ');
                } else {
                    out.push('>');
                }
            }
            '&' if !in_tag => {
                // 收集实体（最长 10 字符）
                let mut entity = String::new();
                let mut terminated = false;
                while let Some(&next) = chars.peek() {
                    if next == ';' {
                        chars.next();
                        terminated = true;
                        break;
                    }
                    if entity.len() >= 10 || next == '&' || next == '<' {
                        break;
                    }
                    entity.push(next);
                    chars.next();
                }
                if terminated {
                    match entity.as_str() {
                        "amp" => out.push('&'),
                        "lt" => out.push('<'),
                        "gt" => out.push('>'),
                        "quot" => out.push('"'),
                        "apos" | "#39" => out.push('\''),
                        "nbsp" | "#160" => out.push(' '),
                        "hellip" => out.push('…'),
                        "mdash" => out.push('—'),
                        "ndash" => out.push('–'),
                        other => {
                            if let Some(num) = other.strip_prefix("#x").or(other.strip_prefix("#X"))
                            {
                                if let Ok(code) = u32::from_str_radix(num, 16) {
                                    if let Some(ch) = char::from_u32(code) {
                                        out.push(ch);
                                    }
                                }
                            } else if let Some(num) = other.strip_prefix('#') {
                                if let Ok(code) = num.parse::<u32>() {
                                    if let Some(ch) = char::from_u32(code) {
                                        out.push(ch);
                                    }
                                }
                            } else {
                                out.push('&');
                                out.push_str(other);
                                out.push(';');
                            }
                        }
                    }
                } else {
                    out.push('&');
                    out.push_str(&entity);
                }
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 按字符数截断（不会切断多字节字符）
pub fn truncate_chars(input: &str, max_chars: usize) -> String {
    if input.chars().count() <= max_chars {
        return input.to_string();
    }
    input.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_url_strips_tracking_and_sorts() {
        let url =
            canonical_url("https://Example.com/Post/1?utm_source=x&b=2&a=1&fbclid=abc#section")
                .unwrap();
        assert_eq!(url, "https://example.com/Post/1?a=1&b=2");
    }

    #[test]
    fn canonical_url_rejects_non_http() {
        assert!(canonical_url("ftp://example.com/x").is_none());
        assert!(canonical_url("not a url").is_none());
        assert!(canonical_url("").is_none());
    }

    #[test]
    fn canonical_url_trims_trailing_slash() {
        assert_eq!(
            canonical_url("https://example.com/a/b/").unwrap(),
            "https://example.com/a/b"
        );
        assert_eq!(
            canonical_url("https://example.com/").unwrap(),
            "https://example.com/"
        );
    }

    #[test]
    fn dedup_key_priority() {
        // 有 URL → url:
        let f = fingerprint(1, "Hello", Some("https://a.com/x"), None, None);
        assert!(f.dedup_key.starts_with("url:"));
        // 无 URL 有标题 → source_title:
        let f = fingerprint(7, "Hello World", None, None, None);
        assert!(f.dedup_key.starts_with("source_title:7:"));
        // 全空 → empty:
        let f = fingerprint(9, "", None, None, None);
        assert_eq!(f.dedup_key, "empty:9");
    }

    #[test]
    fn same_title_different_source_differs() {
        let a = fingerprint(1, "Same Title", None, None, None);
        let b = fingerprint(2, "Same Title", None, None, None);
        assert_ne!(a.dedup_key, b.dedup_key);
        assert_eq!(a.title_hash, b.title_hash);
    }

    #[test]
    fn content_hash_requires_min_length() {
        assert!(content_hash("t", Some("short"), None).is_none());
        let long = "x".repeat(50);
        assert!(content_hash("t", Some(&long), None).is_some());
        // 正文优先于摘要
        let h1 = content_hash(
            "t",
            Some(&long),
            Some("another summary that is long enough ......"),
        );
        let h2 = content_hash("t", Some(&long), None);
        assert_eq!(h1, h2);
    }

    #[test]
    fn strip_html_basic() {
        assert_eq!(
            strip_html("<p>Hello <b>world</b> &amp; friends</p>"),
            "Hello world & friends"
        );
        assert_eq!(strip_html("a &lt;tag&gt; &#20013;&#x6587;"), "a <tag> 中文");
        assert_eq!(strip_html("no entities & bare"), "no entities & bare");
    }

    #[test]
    fn normalized_title_collapses() {
        assert_eq!(normalized_title("  Hello\n  WORLD  "), "hello world");
    }
}
