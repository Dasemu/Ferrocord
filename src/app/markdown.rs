//! Discord-flavoured markdown parser.
//!
//! Produces a small AST that `ui::markdown` renders with iced rich text.
//! Supported syntax (matching the official client):
//!
//! * inline: `**bold**`, `*italic*` / `_italic_`, `__underline__`,
//!   `~~strike~~`, `||spoiler||`, `` `code` `` / ``` ``code`` ```,
//!   `***bold italic***`, `\` escapes, masked links `[label](https://..)`,
//!   autolinks `https://..` and `<https://..>`, mentions `<@id>` `<@!id>`
//!   `<@&id>` `<#id>`, `@everyone` `@here`, custom emoji `<:name:id>` /
//!   `<a:name:id>`, timestamps `<t:unix:style>`;
//! * blocks: fenced code blocks with optional language, `> quote`,
//!   `>>> multi-line quote`, `#`/`##`/`###` headings, `-#` subtext and
//!   `-`/`*` bullet lists.

use crate::net::model::Snowflake;

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Bold(Vec<Inline>),
    Italic(Vec<Inline>),
    Underline(Vec<Inline>),
    Strike(Vec<Inline>),
    Spoiler(Vec<Inline>),
    Code(String),
    Link {
        url: String,
        label: Vec<Inline>,
    },
    UserMention(Snowflake),
    RoleMention(Snowflake),
    ChannelMention(Snowflake),
    Everyone,
    Here,
    Emoji {
        name: String,
        id: Snowflake,
        animated: bool,
    },
    Timestamp {
        unix: i64,
        style: char,
    },
    LineBreak,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Subtext(Vec<Inline>),
    CodeBlock { lang: Option<String>, code: String },
    Quote(Vec<Block>),
    List(Vec<Vec<Inline>>),
}

/// Parses a full message body.
pub fn parse(src: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let mut rest = src;
    while !rest.is_empty() {
        match rest.find("```") {
            Some(start) => {
                let after = &rest[start + 3..];
                match after.find("```") {
                    Some(end) if end > 0 => {
                        parse_lines(&rest[..start], &mut out);
                        out.push(code_block(&after[..end]));
                        rest = &after[end + 3..];
                        // A newline right after the closing fence belongs to it.
                        rest = rest.strip_prefix('\n').unwrap_or(rest);
                    }
                    _ => {
                        parse_lines(rest, &mut out);
                        rest = "";
                    }
                }
            }
            None => {
                parse_lines(rest, &mut out);
                rest = "";
            }
        }
    }
    out
}

fn code_block(inner: &str) -> Block {
    if let Some((first, body)) = inner.split_once('\n') {
        let first_trim = first.trim();
        if !first_trim.is_empty()
            && !first_trim.contains(char::is_whitespace)
            && first_trim
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-#._".contains(c))
            && !body.trim().is_empty()
        {
            return Block::CodeBlock {
                lang: Some(first_trim.to_ascii_lowercase()),
                code: body.trim_end_matches('\n').to_owned(),
            };
        }
        if first_trim.is_empty() {
            return Block::CodeBlock {
                lang: None,
                code: body.trim_end_matches('\n').to_owned(),
            };
        }
    }
    Block::CodeBlock {
        lang: None,
        code: inner.trim_matches('\n').to_owned(),
    }
}

fn parse_lines(text: &str, out: &mut Vec<Block>) {
    let lines: Vec<&str> = text.split('\n').collect();
    // `split` yields a trailing "" for text ending in '\n'; drop it.
    let n = if text.ends_with('\n') {
        lines.len() - 1
    } else {
        lines.len()
    };
    let mut para: Vec<&str> = Vec::new();
    let flush = |para: &mut Vec<&str>, out: &mut Vec<Block>| {
        if !para.is_empty() {
            let joined = para.join("\n");
            if !joined.is_empty() {
                out.push(Block::Paragraph(parse_inline(&joined)));
            }
            para.clear();
        }
    };
    let mut i = 0;
    while i < n {
        let line = lines[i];
        if let Some(q) = line.strip_prefix(">>> ") {
            flush(&mut para, out);
            let mut inner = String::from(q);
            for l in &lines[i + 1..n] {
                inner.push('\n');
                inner.push_str(l);
            }
            out.push(Block::Quote(parse(&inner)));
            return;
        }
        if line.starts_with("> ") || line == ">" {
            flush(&mut para, out);
            let mut inner = Vec::new();
            while i < n && (lines[i].starts_with("> ") || lines[i] == ">") {
                inner.push(lines[i].strip_prefix("> ").unwrap_or(""));
                i += 1;
            }
            out.push(Block::Quote(parse(&inner.join("\n"))));
            continue;
        }
        let heading = [("### ", 3u8), ("## ", 2), ("# ", 1)]
            .iter()
            .find_map(|(p, lvl)| line.strip_prefix(p).map(|r| (*lvl, r)));
        if let Some((lvl, rest)) = heading {
            flush(&mut para, out);
            out.push(Block::Heading(lvl, parse_inline(rest.trim())));
            i += 1;
            continue;
        }
        if let Some(rest) = line.strip_prefix("-# ") {
            flush(&mut para, out);
            out.push(Block::Subtext(parse_inline(rest)));
            i += 1;
            continue;
        }
        if is_list_item(line) {
            flush(&mut para, out);
            let mut items = Vec::new();
            while i < n && is_list_item(lines[i]) {
                items.push(parse_inline(lines[i].trim_start()[2..].trim_start()));
                i += 1;
            }
            out.push(Block::List(items));
            continue;
        }
        para.push(line);
        i += 1;
    }
    flush(&mut para, out);
}

fn is_list_item(line: &str) -> bool {
    let t = line.trim_start();
    (t.starts_with("- ") || t.starts_with("* ")) && t.len() > 2
}

/// Parses inline markdown. Newlines become [`Inline::LineBreak`].
pub fn parse_inline(src: &str) -> Vec<Inline> {
    let mut out = Vec::new();
    let mut text = String::new();
    let b = src.as_bytes();
    let mut i = 0;

    macro_rules! push {
        ($node:expr) => {{
            if !text.is_empty() {
                out.push(Inline::Text(std::mem::take(&mut text)));
            }
            out.push($node);
        }};
    }

    while i < b.len() {
        let rest = &src[i..];
        let c = b[i];

        // Escapes.
        if c == b'\\' && i + 1 < b.len() && b[i + 1].is_ascii_punctuation() {
            text.push(b[i + 1] as char);
            i += 2;
            continue;
        }
        if c == b'\n' {
            push!(Inline::LineBreak);
            i += 1;
            continue;
        }
        // Inline code.
        if c == b'`' {
            let ticks = if rest.starts_with("``") { 2 } else { 1 };
            let delim = &rest[..ticks];
            if let Some(end) = rest[ticks..].find(delim) {
                if end > 0 {
                    let code = &rest[ticks..ticks + end];
                    let code = if ticks == 2 { code.trim() } else { code };
                    push!(Inline::Code(code.to_owned()));
                    i += ticks + end + ticks;
                    continue;
                }
            }
        }
        // Angle-bracket constructs.
        if c == b'<' {
            if let Some((node, len)) = angle(rest) {
                push!(node);
                i += len;
                continue;
            }
        }
        if c == b'@' {
            let prev_ok = i == 0 || !b[i - 1].is_ascii_alphanumeric();
            if prev_ok && rest.starts_with("@everyone") {
                push!(Inline::Everyone);
                i += 9;
                continue;
            }
            if prev_ok && rest.starts_with("@here") {
                push!(Inline::Here);
                i += 5;
                continue;
            }
        }
        // Paired delimiters, longest first.
        if rest.starts_with("***") {
            if let Some(end) = find_closing(rest, 3, "***") {
                push!(Inline::Bold(vec![Inline::Italic(parse_inline(
                    &rest[3..end]
                ))]));
                i += end + 3;
                continue;
            }
        }
        let pairs: [(&str, fn(Vec<Inline>) -> Inline); 4] = [
            ("||", Inline::Spoiler),
            ("**", Inline::Bold),
            ("__", Inline::Underline),
            ("~~", Inline::Strike),
        ];
        let mut matched = false;
        for (d, ctor) in pairs {
            if rest.starts_with(d) {
                if let Some(end) = find_closing(rest, 2, d) {
                    push!(ctor(parse_inline(&rest[2..end])));
                    i += end + 2;
                    matched = true;
                    break;
                }
            }
        }
        if matched {
            continue;
        }
        if c == b'*' && b.get(i + 1).is_some_and(|n| !n.is_ascii_whitespace()) {
            if let Some(end) = find_closing(rest, 1, "*") {
                if !rest[..end].ends_with(char::is_whitespace) {
                    push!(Inline::Italic(parse_inline(&rest[1..end])));
                    i += end + 1;
                    continue;
                }
            }
        }
        if c == b'_' && (i == 0 || !b[i - 1].is_ascii_alphanumeric()) {
            if let Some(end) = find_closing(rest, 1, "_") {
                let after = rest.as_bytes().get(end + 1);
                if after.is_none_or(|a| !a.is_ascii_alphanumeric()) {
                    push!(Inline::Italic(parse_inline(&rest[1..end])));
                    i += end + 1;
                    continue;
                }
            }
        }
        // Masked links.
        if c == b'[' {
            if let Some((node, len)) = masked_link(rest) {
                push!(node);
                i += len;
                continue;
            }
        }
        // Autolinks.
        if (c == b'h' || c == b'H') && (rest.starts_with("https://") || rest.starts_with("http://"))
        {
            let len = autolink_len(rest);
            if len > 8 {
                let url = &rest[..len];
                push!(Inline::Link {
                    url: url.to_owned(),
                    label: vec![Inline::Text(url.to_owned())],
                });
                i += len;
                continue;
            }
        }
        // Literal character.
        let ch = rest.chars().next().unwrap_or(' ');
        text.push(ch);
        i += ch.len_utf8();
    }
    if !text.is_empty() {
        out.push(Inline::Text(text));
    }
    out
}

/// Finds the byte offset of the closing `delim` in `s`, searching from
/// `from`. Skips escapes and inline code; content must be non-empty.
fn find_closing(s: &str, from: usize, delim: &str) -> Option<usize> {
    let b = s.as_bytes();
    let mut j = from;
    while j < b.len() {
        if b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == b'`' && delim != "`" {
            if let Some(end) = s[j + 1..].find('`') {
                j += end + 2;
                continue;
            }
        }
        if s[j..].starts_with(delim) {
            // A single `*` must not close on half of a `**`.
            if delim == "*" && s[j..].starts_with("**") {
                // Skip the `**...**` run entirely if it is paired.
                if let Some(e) = find_closing(s, j + 2, "**") {
                    j = e + 2;
                    continue;
                }
                if j > from {
                    return Some(j);
                }
            }
            if j > from {
                return Some(j);
            }
        }
        j += s[j..].chars().next().map_or(1, char::len_utf8);
    }
    None
}

fn parse_snowflake(s: &str) -> Option<Snowflake> {
    if s.is_empty() || s.len() > 20 || !s.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    s.parse().ok().map(Snowflake)
}

/// `<...>` constructs. Returns the node and the consumed length.
fn angle(s: &str) -> Option<(Inline, usize)> {
    let end = s.find('>')?;
    let inner = &s[1..end];
    let len = end + 1;
    if let Some(r) = inner.strip_prefix("@&") {
        return parse_snowflake(r).map(|id| (Inline::RoleMention(id), len));
    }
    if let Some(r) = inner.strip_prefix("@!").or_else(|| inner.strip_prefix('@')) {
        return parse_snowflake(r).map(|id| (Inline::UserMention(id), len));
    }
    if let Some(r) = inner.strip_prefix('#') {
        return parse_snowflake(r).map(|id| (Inline::ChannelMention(id), len));
    }
    if let Some(r) = inner.strip_prefix("t:") {
        let (unix, style) = match r.split_once(':') {
            Some((u, st)) if st.len() == 1 => (u, st.chars().next().unwrap_or('f')),
            Some(_) => return None,
            None => (r, 'f'),
        };
        let unix: i64 = unix.parse().ok()?;
        return Some((Inline::Timestamp { unix, style }, len));
    }
    let (animated, emoji) = if let Some(r) = inner.strip_prefix("a:") {
        (true, Some(r))
    } else if let Some(r) = inner.strip_prefix(':') {
        (false, Some(r))
    } else {
        (false, None)
    };
    if let Some(r) = emoji {
        let (name, id) = r.split_once(':')?;
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        return parse_snowflake(id).map(|id| {
            (
                Inline::Emoji {
                    name: name.to_owned(),
                    id,
                    animated,
                },
                len,
            )
        });
    }
    if (inner.starts_with("https://") || inner.starts_with("http://"))
        && !inner.contains(char::is_whitespace)
    {
        return Some((
            Inline::Link {
                url: inner.to_owned(),
                label: vec![Inline::Text(inner.to_owned())],
            },
            len,
        ));
    }
    None
}

fn masked_link(s: &str) -> Option<(Inline, usize)> {
    let close = find_closing(s, 1, "]")?;
    let rest = &s[close + 1..];
    let after = rest.strip_prefix('(')?;
    let end = after.find(')')?;
    let url = after[..end].trim();
    let url = url.trim_start_matches('<').trim_end_matches('>');
    if !(url.starts_with("https://") || url.starts_with("http://")) || url.contains(' ') {
        return None;
    }
    let label = &s[1..close];
    Some((
        Inline::Link {
            url: url.to_owned(),
            label: parse_inline(label),
        },
        close + 1 + 1 + end + 1,
    ))
}

fn autolink_len(s: &str) -> usize {
    let mut end = s
        .find(|c: char| c.is_whitespace() || c == '<' || c == '>' || c == '"')
        .unwrap_or(s.len());
    // Trim trailing punctuation, keeping balanced parentheses.
    loop {
        let Some(last) = s[..end].chars().last() else {
            break;
        };
        let trim = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' | '*' | '_' | '~' | '|' => true,
            ')' => s[..end].matches('(').count() < s[..end].matches(')').count(),
            _ => false,
        };
        if trim {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Flattens markdown to plain text (for notifications and previews).
pub fn to_plain(blocks: &[Block], resolve: &dyn Fn(&Inline) -> Option<String>) -> String {
    fn inl(v: &[Inline], r: &dyn Fn(&Inline) -> Option<String>, out: &mut String) {
        for i in v {
            match i {
                Inline::Text(t) | Inline::Code(t) => out.push_str(t),
                Inline::Bold(c) | Inline::Italic(c) | Inline::Underline(c) | Inline::Strike(c) => {
                    inl(c, r, out)
                }
                Inline::Spoiler(_) => out.push_str("[spoiler]"),
                Inline::Link { label, .. } => inl(label, r, out),
                Inline::LineBreak => out.push(' '),
                Inline::Everyone => out.push_str("@everyone"),
                Inline::Here => out.push_str("@here"),
                Inline::Emoji { name, .. } => {
                    out.push(':');
                    out.push_str(name);
                    out.push(':');
                }
                other => out.push_str(&r(other).unwrap_or_default()),
            }
        }
    }
    let mut out = String::new();
    for b in blocks {
        if !out.is_empty() {
            out.push(' ');
        }
        match b {
            Block::Paragraph(v) | Block::Heading(_, v) | Block::Subtext(v) => {
                inl(v, resolve, &mut out)
            }
            Block::CodeBlock { code, .. } => out.push_str(code),
            Block::Quote(inner) => out.push_str(&to_plain(inner, resolve)),
            Block::List(items) => {
                for it in items {
                    inl(it, resolve, &mut out);
                    out.push(' ');
                }
            }
        }
    }
    out.trim().to_owned()
}

/// True when the message consists only of custom emoji (and whitespace),
/// at most 30 of them: the official client renders those "jumbo".
pub fn is_emoji_only(blocks: &[Block]) -> bool {
    let [Block::Paragraph(v)] = blocks else {
        return false;
    };
    let mut count = 0;
    for i in v {
        match i {
            Inline::Emoji { .. } => count += 1,
            Inline::Text(t) if t.trim().is_empty() => {}
            _ => return false,
        }
    }
    (1..=30).contains(&count)
}

#[cfg(test)]
mod tests {
    use super::Inline::*;
    use super::*;

    fn t(s: &str) -> Inline {
        Text(s.into())
    }

    #[test]
    fn plain_text() {
        assert_eq!(parse_inline("hello world"), vec![t("hello world")]);
    }

    #[test]
    fn bold_italic_underline_strike() {
        assert_eq!(
            parse_inline("**b** *i* __u__ ~~s~~ _i2_"),
            vec![
                Bold(vec![t("b")]),
                t(" "),
                Italic(vec![t("i")]),
                t(" "),
                Underline(vec![t("u")]),
                t(" "),
                Strike(vec![t("s")]),
                t(" "),
                Italic(vec![t("i2")]),
            ]
        );
    }

    #[test]
    fn nested_and_triple() {
        assert_eq!(
            parse_inline("***both***"),
            vec![Bold(vec![Italic(vec![t("both")])])]
        );
        assert_eq!(
            parse_inline("*a **b** c*"),
            vec![Italic(vec![t("a "), Bold(vec![t("b")]), t(" c")])]
        );
        assert_eq!(
            parse_inline("__**x**__"),
            vec![Underline(vec![Bold(vec![t("x")])])]
        );
    }

    #[test]
    fn unclosed_delimiters_are_literal() {
        assert_eq!(parse_inline("2 * 3 = 6"), vec![t("2 * 3 = 6")]);
        assert_eq!(parse_inline("**open"), vec![t("**open")]);
        assert_eq!(parse_inline("snake_case_name"), vec![t("snake_case_name")]);
    }

    #[test]
    fn escapes() {
        assert_eq!(parse_inline(r"\*not italic\*"), vec![t("*not italic*")]);
    }

    #[test]
    fn inline_code_wins_over_formatting() {
        assert_eq!(
            parse_inline("use `**raw**` here"),
            vec![t("use "), Code("**raw**".into()), t(" here")]
        );
        assert_eq!(parse_inline("``a`b``"), vec![Code("a`b".into())]);
    }

    #[test]
    fn spoilers() {
        assert_eq!(
            parse_inline("||secret **bold**||"),
            vec![Spoiler(vec![t("secret "), Bold(vec![t("bold")])])]
        );
    }

    #[test]
    fn mentions_and_emoji() {
        assert_eq!(
            parse_inline("hi <@123> <@!45> <@&6> <#7> @everyone @here <:pog:89> <a:dance:90>"),
            vec![
                t("hi "),
                UserMention(Snowflake(123)),
                t(" "),
                UserMention(Snowflake(45)),
                t(" "),
                RoleMention(Snowflake(6)),
                t(" "),
                ChannelMention(Snowflake(7)),
                t(" "),
                Everyone,
                t(" "),
                Here,
                t(" "),
                Emoji {
                    name: "pog".into(),
                    id: Snowflake(89),
                    animated: false
                },
                t(" "),
                Emoji {
                    name: "dance".into(),
                    id: Snowflake(90),
                    animated: true
                },
            ]
        );
        assert_eq!(parse_inline("mail@everyone.com")[0], t("mail@everyone.com"));
        assert_eq!(parse_inline("<@abc>"), vec![t("<@abc>")]);
    }

    #[test]
    fn timestamps() {
        assert_eq!(
            parse_inline("<t:1700000000:R>"),
            vec![Timestamp {
                unix: 1700000000,
                style: 'R'
            }]
        );
        assert_eq!(
            parse_inline("<t:1700000000>"),
            vec![Timestamp {
                unix: 1700000000,
                style: 'f'
            }]
        );
    }

    #[test]
    fn links() {
        assert_eq!(
            parse_inline("see https://example.com/a_(b). ok"),
            vec![
                t("see "),
                Link {
                    url: "https://example.com/a_(b)".into(),
                    label: vec![t("https://example.com/a_(b)")]
                },
                t(". ok")
            ]
        );
        assert_eq!(
            parse_inline("[the **docs**](https://docs.rs)"),
            vec![Link {
                url: "https://docs.rs".into(),
                label: vec![t("the "), Bold(vec![t("docs")])]
            }]
        );
        assert_eq!(
            parse_inline("<https://no.embed>"),
            vec![Link {
                url: "https://no.embed".into(),
                label: vec![t("https://no.embed")]
            }]
        );
        // Non-http masked links are not links.
        assert_eq!(
            parse_inline("[x](javascript:alert)"),
            vec![t("[x](javascript:alert)")]
        );
    }

    #[test]
    fn line_breaks() {
        assert_eq!(parse_inline("a\nb"), vec![t("a"), LineBreak, t("b")]);
    }

    #[test]
    fn code_blocks() {
        assert_eq!(
            parse("before\n```rust\nfn main() {}\n```\nafter"),
            vec![
                Block::Paragraph(vec![t("before")]),
                Block::CodeBlock {
                    lang: Some("rust".into()),
                    code: "fn main() {}".into()
                },
                Block::Paragraph(vec![t("after")]),
            ]
        );
        assert_eq!(
            parse("```just code```"),
            vec![Block::CodeBlock {
                lang: None,
                code: "just code".into()
            }]
        );
        // Formatting is not applied inside code blocks.
        assert_eq!(
            parse("```\n**x**\n```"),
            vec![Block::CodeBlock {
                lang: None,
                code: "**x**".into()
            }]
        );
    }

    #[test]
    fn quotes() {
        assert_eq!(
            parse("> quoted **b**\n> second\nnormal"),
            vec![
                Block::Quote(vec![Block::Paragraph(vec![
                    t("quoted "),
                    Bold(vec![t("b")]),
                    LineBreak,
                    t("second")
                ])]),
                Block::Paragraph(vec![t("normal")]),
            ]
        );
        assert_eq!(
            parse("intro\n>>> all\nof this"),
            vec![
                Block::Paragraph(vec![t("intro")]),
                Block::Quote(vec![Block::Paragraph(vec![
                    t("all"),
                    LineBreak,
                    t("of this")
                ])]),
            ]
        );
    }

    #[test]
    fn headings_subtext_lists() {
        assert_eq!(
            parse("# Big\n## Mid\n-# small\n- one\n- two"),
            vec![
                Block::Heading(1, vec![t("Big")]),
                Block::Heading(2, vec![t("Mid")]),
                Block::Subtext(vec![t("small")]),
                Block::List(vec![vec![t("one")], vec![t("two")]]),
            ]
        );
    }

    #[test]
    fn unicode_is_preserved() {
        assert_eq!(
            parse_inline("héllo **wörld** 🦀"),
            vec![t("héllo "), Bold(vec![t("wörld")]), t(" 🦀")]
        );
    }

    #[test]
    fn plain_and_emoji_only() {
        let b = parse("**hi** ||x|| <:a:1>");
        assert_eq!(to_plain(&b, &|_| None), "hi [spoiler] :a:");
        assert!(is_emoji_only(&parse("<:a:1> <:b:2>")));
        assert!(!is_emoji_only(&parse("text <:a:1>")));
    }

    #[test]
    fn never_panics_on_adversarial_input() {
        for s in [
            "", "*", "**", "***", "||", "`", "``", "```", "<", "<@", "<t:", "[", "[](", "\\",
            "_ _", "~~~", "> ", ">>> ", "# ", "-# ", "- ", "<:x:>", "<a:>", "é*é", "**é", "\n\n",
        ] {
            let _ = parse(s);
            let _ = parse_inline(s);
        }
    }
}
