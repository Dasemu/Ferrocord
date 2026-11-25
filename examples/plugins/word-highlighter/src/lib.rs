//! Example plugin: message decorations from observed messages.

use ferrocord_plugin_sdk::prelude::*;

#[derive(Default)]
pub struct WordHighlighter;

/// The configured words found in `content` (whole words, case-insensitive).
pub fn matches(content: &str, words: &str) -> Vec<String> {
    let lower = content.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    words
        .split(',')
        .map(|w| w.trim().to_lowercase())
        .filter(|w| !w.is_empty() && tokens.contains(&w.as_str()))
        .collect()
}

fn check(id: u64, content: &str) {
    let words = host::setting("words")
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let found = matches(content, &words);
    let kind = match host::setting("style").as_ref().and_then(|v| v.as_str()) {
        Some("badge") => DecorationKind::Badge,
        _ => DecorationKind::Highlight,
    };
    let decorations = if found.is_empty() {
        Vec::new()
    } else {
        vec![Decoration {
            kind,
            text: found.join(", "),
            tooltip: Some("Matched by Word Highlighter".into()),
        }]
    };
    let _ = host::decorate(id, decorations);
}

impl Plugin for WordHighlighter {
    fn on_event(&mut self, event: Event) {
        match event {
            Event::MessageCreate { message } => check(message.id, &message.content),
            Event::MessageUpdate {
                id,
                content: Some(c),
                ..
            } => check(id, &c),
            Event::History { messages, .. } => {
                for m in messages {
                    check(m.id, &m.content);
                }
            }
            _ => {}
        }
    }
}

register_plugin!(WordHighlighter);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whole_words_only() {
        assert_eq!(matches("I love Rust!", "rust, go"), ["rust"]);
        assert!(matches("rusty", "rust").is_empty());
        assert!(matches("anything", " , ").is_empty());
    }
}
