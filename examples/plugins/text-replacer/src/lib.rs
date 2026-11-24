//! Example plugin: outgoing text transform plus one command.

use ferrocord_plugin_sdk::prelude::*;

#[derive(Default)]
pub struct TextReplacer;

const SHORTCUTS: [(&str, &str); 3] = [
    ("/shrug", r"¯\\\_(ツ)\_/¯"),
    ("/tableflip", "(╯°□°)╯︵ ┻━┻"),
    ("/unflip", "┬─┬ノ( º _ ºノ)"),
];

/// Applies the shortcuts and `from=>to` rules to `text`.
pub fn rewrite(text: &str, shortcuts: bool, rules: &str) -> Option<String> {
    let mut out = text.to_owned();
    if shortcuts {
        for (cmd, face) in SHORTCUTS {
            let rest = match out.strip_prefix(cmd) {
                Some(r) if r.is_empty() || r.starts_with(' ') => r.trim(),
                _ => continue,
            };
            out = if rest.is_empty() {
                face.to_owned()
            } else {
                format!("{rest} {face}")
            };
            break;
        }
    }
    for rule in rules.split(';') {
        if let Some((from, to)) = rule.split_once("=>") {
            let from = from.trim();
            if !from.is_empty() {
                out = out.replace(from, to.trim());
            }
        }
    }
    (out != text).then_some(out)
}

impl Plugin for TextReplacer {
    fn transform(&mut self, input: TransformInput) -> Option<String> {
        let shortcuts = host::setting("shortcuts")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let rules = host::setting("rules")
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        rewrite(&input.text, shortcuts, &rules)
    }

    fn command(&mut self, input: CommandInput) {
        if input.name == "lenny" {
            let args = input.args.trim();
            let text = if args.is_empty() {
                "( ͡° ͜ʖ ͡°)".to_owned()
            } else {
                format!("{args} ( ͡° ͜ʖ ͡°)")
            };
            if host::send_message(&text).is_err() {
                let _ = host::toast("Text Replacer could not send the message.");
            }
        }
    }
}

register_plugin!(TextReplacer);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcuts_and_rules() {
        assert_eq!(
            rewrite("/shrug", true, "").as_deref(),
            Some(r"¯\\\_(ツ)\_/¯")
        );
        assert_eq!(
            rewrite("/tableflip ok", true, "").as_deref(),
            Some("ok (╯°□°)╯︵ ┻━┻")
        );
        assert_eq!(rewrite("/shrugged", true, ""), None);
        assert_eq!(rewrite("/shrug", false, ""), None);
        assert_eq!(rewrite("a --> b", true, "-->=>→").as_deref(), Some("a → b"));
        assert_eq!(rewrite("plain", true, "-->=>→"), None);
    }
}
