//! Example plugin: message context-menu actions.

use ferrocord_plugin_sdk::prelude::*;

#[derive(Default)]
pub struct QuoteMessage;

/// `> line` for every line, then `— author`.
pub fn quote(author: &str, content: &str) -> String {
    let mut out = String::new();
    for line in content.lines() {
        out.push_str("> ");
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("> — ");
    out.push_str(author);
    out.push('\n');
    out
}

impl Plugin for QuoteMessage {
    fn action(&mut self, input: ActionInput) {
        let q = quote(&input.message.author_name, &input.message.content);
        let r = match input.action.as_str() {
            "copy-quote" => host::clipboard_write(&q),
            "quote-composer" => host::composer_insert(&q),
            _ => return,
        };
        if r.is_ok() && input.action == "copy-quote" {
            let _ = host::toast("Quote copied.");
        }
    }
}

register_plugin!(QuoteMessage);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_every_line() {
        assert_eq!(quote("Ann", "a\nb"), "> a\n> b\n> — Ann\n");
    }
}
