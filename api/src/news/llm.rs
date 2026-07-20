//! LLM 客户端（OpenAI 兼容 /chat/completions）与中文日报出版契约解析
use serde::{Deserialize, Serialize};

// ---------- 出版契约（单次调用产出中文日报） ----------

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DigestDoc {
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub sections: Vec<DigestSection>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DigestSection {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub items: Vec<DigestItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DigestItem {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub why: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default = "default_importance")]
    pub importance: i32,
    #[serde(default)]
    pub tags: Vec<String>,
    /// 文章内锚点 id（生成阶段填充，不要求 LLM 输出）
    #[serde(default)]
    pub anchor: String,
}

fn default_importance() -> i32 {
    1
}

impl DigestDoc {
    /// 清洗：丢弃无标题条目与空分节，钳制 importance，按顺序填充锚点
    pub fn sanitize(&mut self) {
        for section in &mut self.sections {
            section.items.retain(|item| !item.title.trim().is_empty());
        }
        self.sections.retain(|s| !s.items.is_empty());
        let mut counter = 0;
        for section in &mut self.sections {
            for item in &mut section.items {
                counter += 1;
                item.importance = item.importance.clamp(1, 5);
                item.anchor = format!("intel-{counter}");
                if item.tags.len() > 5 {
                    item.tags.truncate(5);
                }
            }
        }
    }

    pub fn item_count(&self) -> usize {
        self.sections.iter().map(|s| s.items.len()).sum()
    }

    /// 全部条目按 importance 降序（同级保持文档顺序）的引用列表
    pub fn items_by_importance(&self) -> Vec<&DigestItem> {
        let mut items: Vec<&DigestItem> =
            self.sections.iter().flat_map(|s| s.items.iter()).collect();
        items.sort_by_key(|item| std::cmp::Reverse(item.importance));
        items
    }
}

// ---------- 健壮 JSON 提取 ----------

/// 从 LLM 输出中提取首个合法 JSON 对象：
/// 依次尝试整体解析 → 剥离 ``` 围栏 → 截取首个 `{` 到最后一个 `}`
pub fn extract_json(raw: &str) -> Option<serde_json::Value> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return Some(v);
    }
    // 剥代码围栏 ```json ... ```
    let unfenced = if trimmed.starts_with("```") {
        let inner = trimmed.trim_start_matches("```");
        let inner = inner.strip_prefix("json").unwrap_or(inner);
        inner.trim_end_matches("```").trim()
    } else {
        trimmed
    };
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(unfenced) {
        return Some(v);
    }
    // 截取首个 { 到最后一个 }
    let start = unfenced.find('{')?;
    let end = unfenced.rfind('}')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&unfenced[start..=end]).ok()
}

/// 解析出版契约；结构不符（无有效条目）返回 None
pub fn parse_digest_doc(raw: &str) -> Option<DigestDoc> {
    let value = extract_json(raw)?;
    let mut doc: DigestDoc = serde_json::from_value(value).ok()?;
    doc.sanitize();
    if doc.sections.is_empty() {
        return None;
    }
    Some(doc)
}

// ---------- OpenAI 兼容调用 ----------

pub struct LlmRequest<'a> {
    pub base_url: &'a str,
    pub api_key: &'a str,
    pub model: &'a str,
    pub system_prompt: &'a str,
    pub user_prompt: &'a str,
}

/// 调用 chat/completions，返回首个 choice 的文本内容
pub async fn chat(req: &LlmRequest<'_>) -> anyhow::Result<String> {
    let url = format!("{}/chat/completions", req.base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": req.model,
        "messages": [
            { "role": "system", "content": req.system_prompt },
            { "role": "user", "content": req.user_prompt },
        ],
        "temperature": 0.3,
    });
    let (status, text) = crate::outbound::post_json(
        &url,
        req.api_key,
        &body,
        2 * 1024 * 1024,
        std::time::Duration::from_secs(600),
    )
    .await?;
    if !status.is_success() {
        let brief: String = text.chars().take(500).collect();
        anyhow::bail!("llm http {status}: {brief}");
    }
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| anyhow::anyhow!("llm response not json: {e}"))?;
    let content = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("llm response missing choices[0].message.content"))?;
    Ok(content.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_plain_json() {
        let v = extract_json(r#"{"a": 1}"#).unwrap();
        assert_eq!(v["a"], 1);
    }

    #[test]
    fn extract_fenced_json() {
        let raw = "```json\n{\"a\": 2}\n```";
        assert_eq!(extract_json(raw).unwrap()["a"], 2);
        let raw = "```\n{\"a\": 3}\n```";
        assert_eq!(extract_json(raw).unwrap()["a"], 3);
    }

    #[test]
    fn extract_embedded_json() {
        let raw = "好的，以下是结果：\n{\"a\": {\"b\": 4}}\n希望有帮助";
        assert_eq!(extract_json(raw).unwrap()["a"]["b"], 4);
    }

    #[test]
    fn extract_rejects_garbage() {
        assert!(extract_json("no json here").is_none());
        assert!(extract_json("{ broken").is_none());
    }

    #[test]
    fn parse_doc_sanitizes() {
        let raw = r#"{
            "date": "2026-07-16",
            "title": "AI 前沿日报", "summary": "综述",
            "sections": [
                {"name": "模型", "items": [
                    {"title": "条目一", "importance": 9, "tags": ["a","b","c","d","e","f"]},
                    {"title": ""}
                ]},
                {"name": "空", "items": []}
            ]
        }"#;
        let doc = parse_digest_doc(raw).unwrap();
        assert_eq!(doc.sections.len(), 1);
        assert_eq!(doc.sections[0].items.len(), 1);
        let item = &doc.sections[0].items[0];
        assert_eq!(item.importance, 5);
        assert_eq!(item.anchor, "intel-1");
        assert_eq!(item.tags.len(), 5);
    }

    #[test]
    fn parse_doc_rejects_missing_title() {
        let raw = r#"{"sections": [{"items": [{"summary": "无标题"}]}]}"#;
        assert!(parse_digest_doc(raw).is_none());
    }

    #[test]
    fn parse_doc_rejects_empty() {
        assert!(parse_digest_doc(r#"{"sections": []}"#).is_none());
        assert!(parse_digest_doc("not json").is_none());
    }

    #[test]
    fn importance_ordering() {
        let raw = r#"{"sections": [{"items": [
            {"title": "低", "importance": 1},
            {"title": "高", "importance": 5},
            {"title": "中", "importance": 3}
        ]}]}"#;
        let doc = parse_digest_doc(raw).unwrap();
        let ordered = doc.items_by_importance();
        assert_eq!(ordered[0].title, "高");
        assert_eq!(ordered[1].title, "中");
        assert_eq!(ordered[2].title, "低");
    }
}
