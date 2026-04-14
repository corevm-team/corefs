use crate::domain::metadata::ContentClass;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticAttributes {
    pub attributes: Vec<(String, String)>,
}

#[derive(Debug, Default)]
pub struct SemanticService;

impl SemanticService {
    pub fn analyze(path: &str, bytes: &[u8], content_class: &ContentClass) -> SemanticAttributes {
        match content_class {
            ContentClass::Text | ContentClass::SourceCode => analyze_textual(path, bytes),
            _ => SemanticAttributes {
                attributes: vec![
                    ("semantic.byte_size".to_string(), bytes.len().to_string()),
                    (
                        "semantic.kind".to_string(),
                        content_kind_name(content_class).to_string(),
                    ),
                ],
            },
        }
    }
}

fn analyze_textual(path: &str, bytes: &[u8]) -> SemanticAttributes {
    let text = String::from_utf8_lossy(bytes);
    let trimmed = text.trim();
    let line_count = text
        .lines()
        .count()
        .max(if text.is_empty() { 0 } else { 1 });
    let word_count = text.split_whitespace().count();
    let language = detect_language(path);
    let summary = summarize(trimmed, 160);
    let keywords = extract_keywords(trimmed, 8).join(", ");

    let mut attributes = vec![
        ("semantic.byte_size".to_string(), bytes.len().to_string()),
        ("semantic.lines".to_string(), line_count.to_string()),
        ("semantic.words".to_string(), word_count.to_string()),
        ("semantic.language".to_string(), language.to_string()),
        ("semantic.summary".to_string(), summary),
    ];

    if !keywords.is_empty() {
        attributes.push(("semantic.keywords".to_string(), keywords));
    }

    SemanticAttributes { attributes }
}

fn detect_language(path: &str) -> &'static str {
    if path.ends_with(".rs") {
        "rust"
    } else if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".md") {
        "markdown"
    } else if path.ends_with(".txt") {
        "text"
    } else if path.ends_with(".json") {
        "json"
    } else if path.ends_with(".toml") {
        "toml"
    } else {
        "plain"
    }
}

fn summarize(text: &str, max_chars: usize) -> String {
    if text.is_empty() {
        return String::new();
    }
    if text.chars().count() <= max_chars {
        text.to_string()
    } else {
        format!(
            "{}...",
            text.chars().take(max_chars).collect::<String>().trim_end()
        )
    }
}

fn extract_keywords(text: &str, limit: usize) -> Vec<String> {
    let mut counts = BTreeMap::<String, usize>::new();
    for token in text
        .split(|ch: char| !ch.is_alphanumeric() && ch != '_' && ch != '-')
        .map(|token| token.trim().to_lowercase())
        .filter(|token| token.len() >= 4)
        .filter(|token| !is_stopword(token))
    {
        *counts.entry(token).or_default() += 1;
    }

    let mut items = counts.into_iter().collect::<Vec<_>>();
    items.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    items
        .into_iter()
        .take(limit)
        .map(|(token, _)| token)
        .collect()
}

fn is_stopword(token: &str) -> bool {
    matches!(
        token,
        "this"
            | "that"
            | "with"
            | "from"
            | "have"
            | "into"
            | "your"
            | "core"
            | "file"
            | "files"
            | "und"
            | "oder"
            | "eine"
            | "einer"
            | "einem"
            | "the"
            | "and"
            | "for"
            | "der"
            | "die"
            | "das"
    )
}

fn content_kind_name(content_class: &ContentClass) -> &'static str {
    match content_class {
        ContentClass::Text => "text",
        ContentClass::Binary => "binary",
        ContentClass::Image => "image",
        ContentClass::SourceCode => "source_code",
        ContentClass::Archive => "archive",
        ContentClass::Unknown => "unknown",
    }
}

#[cfg(test)]
#[path = "semantic_tests.rs"]
mod tests;
