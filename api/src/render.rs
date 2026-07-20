use std::collections::HashSet;

use comrak::nodes::NodeValue;
use comrak::{format_html, parse_document, Anchorizer, Arena, Options};
use jieba_rs::Jieba;
use serde::Serialize;

#[derive(Serialize, Clone)]
pub struct TocItem {
    pub level: u8,
    pub id: String,
    pub text: String,
}

pub struct Rendered {
    pub html: String,
    pub toc: Vec<TocItem>,
    pub plain: String,
}

fn make_options() -> Options<'static> {
    let mut opts = Options::default();
    opts.extension.strikethrough = true;
    opts.extension.table = true;
    opts.extension.autolink = true;
    opts.extension.tasklist = true;
    opts.extension.footnotes = true;
    opts.extension.header_id_prefix = Some(String::new());
    // 单管理员撰写的内容视为可信，允许内嵌原始 HTML
    opts.render.r#unsafe = true;
    opts
}

fn sanitize_rendered_html(html: &str) -> String {
    let extra_tags: HashSet<&str> = ["details", "summary", "figure", "figcaption", "mark"]
        .into_iter()
        .collect();
    let generic: HashSet<&str> = ["class", "id", "title", "aria-label"].into_iter().collect();
    let mut builder = ammonia::Builder::default();
    builder
        .add_tags(&extra_tags)
        .add_generic_attributes(&generic);
    builder.clean(html).to_string()
}

/// 渲染 Markdown 为 HTML，同时提取 TOC（h2-h4）与纯文本
pub fn render_markdown(md: &str) -> Rendered {
    let arena = Arena::new();
    let opts = make_options();
    let root = parse_document(&arena, md, &opts);

    let mut anchorizer = Anchorizer::new();
    let mut toc: Vec<TocItem> = Vec::new();
    let mut plain = String::new();

    for node in root.descendants() {
        let heading_level = {
            let data = node.data.borrow();
            match &data.value {
                NodeValue::Heading(heading) => Some(heading.level),
                NodeValue::Text(t) => {
                    plain.push_str(t);
                    plain.push(' ');
                    None
                }
                NodeValue::Code(c) => {
                    plain.push_str(&c.literal);
                    plain.push(' ');
                    None
                }
                _ => None,
            }
        };
        if let Some(level) = heading_level {
            // 与 comrak 渲染逻辑一致：collect_text + anchorize，
            // 且必须对所有标题按文档顺序 anchorize，保证 id 去重序列一致
            let text = node.collect_text();
            let id = anchorizer.anchorize(&text);
            if (2..=4).contains(&level) {
                toc.push(TocItem { level, id, text });
            }
        }
    }

    let mut html = String::new();
    format_html(root, &opts, &mut html).expect("format_html should not fail");

    let html = sanitize_rendered_html(&html);
    Rendered { html, toc, plain }
}

/// 文章内容三件套：HTML、TOC JSON、搜索文本（供后台编辑与日报生成共用）
pub struct PostContent {
    pub html: String,
    pub toc_json: String,
    pub search_text: String,
}

/// 渲染 Markdown 并组装文章所需的派生字段
pub fn prepare_post_content(
    jieba: &Jieba,
    title: &str,
    extra_terms: &[&str],
    markdown: &str,
) -> Result<PostContent, serde_json::Error> {
    let rendered = render_markdown(markdown);
    let toc_json = serde_json::to_string(&rendered.toc)?;
    let mut parts: Vec<&str> = vec![title];
    parts.extend_from_slice(extra_terms);
    parts.push(rendered.plain.as_str());
    let search_text = build_search_text(jieba, &parts);
    Ok(PostContent {
        html: rendered.html,
        toc_json,
        search_text,
    })
}

/// 组装搜索文本：标题 + 标签名 + 正文纯文本，jieba 搜索引擎模式分词，小写化
pub fn build_search_text(jieba: &Jieba, parts: &[&str]) -> String {
    let joined = parts.join(" ");
    let words: Vec<&str> = jieba
        .cut_for_search(&joined, true)
        .into_iter()
        .map(|t| t.word)
        .collect();
    words.join(" ").to_lowercase()
}

/// 将查询串切分为小写检索词（最多 8 个）
pub fn tokenize_query(jieba: &Jieba, q: &str) -> Vec<String> {
    let mut terms: Vec<String> = jieba
        .cut_for_search(q, true)
        .into_iter()
        .map(|t| t.word.trim().to_lowercase())
        .filter(|w| !w.is_empty() && w.chars().any(|c| c.is_alphanumeric()))
        .collect();
    terms.dedup();
    terms.truncate(8);
    terms
}

/// slug 清洗：仅保留小写字母、数字、连字符
pub fn sanitize_slug(input: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in input.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    out
}

#[cfg(test)]
mod security_tests {
    use super::*;

    #[test]
    fn strips_active_html() {
        let rendered =
            render_markdown("<script>alert(1)</script><img src=x onerror=alert(1)><p>safe</p>");
        assert!(!rendered.html.contains("script"));
        assert!(!rendered.html.contains("onerror"));
        assert!(rendered.html.contains("safe"));
    }

    #[test]
    fn strips_dangerous_links() {
        let rendered = render_markdown("[x](javascript:alert(1))");
        assert!(!rendered.html.contains("javascript:"));
    }
}
