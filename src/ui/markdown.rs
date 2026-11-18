//! Renders the markdown AST (`app::markdown`) with iced rich text.

use iced::widget::text::Span;
use iced::widget::{column, container, rich_text, row, span, text};
use iced::{Background, Border, Color, Font, Length, Padding, font};

use super::El;
use super::theme::{self, INTER, MONO};
use crate::app::markdown::{Block, Inline, is_emoji_only, parse};
use crate::app::{App, Message, Ui};
use crate::net::model::MessageId;

#[derive(Clone, Copy)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
    size: f32,
    color: Color,
    spoiler_hidden: bool,
}

struct Ctx<'a> {
    app: &'a App,
    msg: MessageId,
    revealed: bool,
}

/// Renders a message body. `base` is the normal text colour.
pub fn render<'a>(app: &'a App, msg: MessageId, content: &str, base: Color, size: f32) -> El<'a> {
    let blocks = parse(content);
    let ctx = Ctx {
        app,
        msg,
        revealed: app.revealed_spoilers.contains(&msg),
    };
    if is_emoji_only(&blocks) {
        return jumbo(&ctx, &blocks);
    }
    let style = Style {
        bold: false,
        italic: false,
        underline: false,
        strike: false,
        size,
        color: base,
        spoiler_hidden: false,
    };
    blocks_view(&ctx, &blocks, style)
}

fn blocks_view<'a>(ctx: &Ctx<'a>, blocks: &[Block], style: Style) -> El<'a> {
    let p = ctx.app.theme.palette;
    let mut col = column![].spacing(4).width(Length::Fill);
    for b in blocks {
        let el: El<'a> = match b {
            Block::Paragraph(v) => para(ctx, v, style),
            Block::Heading(level, v) => {
                let size = match level {
                    1 => 24.0,
                    2 => 20.0,
                    _ => 16.0,
                };
                para(
                    ctx,
                    v,
                    Style {
                        bold: true,
                        size,
                        color: p.header_text,
                        ..style
                    },
                )
            }
            Block::Subtext(v) => para(
                ctx,
                v,
                Style {
                    size: 12.0,
                    color: p.text_muted,
                    ..style
                },
            ),
            Block::CodeBlock { code, .. } => {
                container(text(code.clone()).font(MONO).size(14).color(p.text_normal))
                    .width(Length::Fill)
                    .padding(8)
                    .style(move |_| container::Style {
                        background: Some(Background::Color(p.code_bg)),
                        border: Border {
                            radius: 4.0.into(),
                            width: 1.0,
                            color: p.border,
                        },
                        ..Default::default()
                    })
                    .into()
            }
            Block::Quote(inner) => row![
                container(text(""))
                    .width(4)
                    .height(Length::Fill)
                    .style(theme::rounded_bg(p.badge_muted, 4.0)),
                container(blocks_view(ctx, inner, style)).padding(Padding::ZERO.left(12)),
            ]
            .height(Length::Shrink)
            .into(),
            Block::List(items) => {
                let mut c = column![].spacing(2);
                for it in items {
                    c = c.push(row![
                        text("•").size(style.size).color(style.color).width(16),
                        para(ctx, it, style)
                    ]);
                }
                c.into()
            }
        };
        col = col.push(el);
    }
    col.into()
}

fn para<'a>(ctx: &Ctx<'a>, v: &[Inline], style: Style) -> El<'a> {
    let mut spans: Vec<Span<'a, Ui, Font>> = Vec::new();
    inline(ctx, v, style, None, &mut spans);
    rich_text(spans)
        .size(style.size)
        .on_link_click(Message::Ui)
        .width(Length::Fill)
        .into()
}

fn font_for(s: &Style) -> Font {
    Font {
        weight: if s.bold {
            font::Weight::Bold
        } else {
            font::Weight::Normal
        },
        style: if s.italic {
            font::Style::Italic
        } else {
            font::Style::Normal
        },
        ..INTER
    }
}

fn push_text<'a>(
    spans: &mut Vec<Span<'a, Ui, Font>>,
    s: String,
    st: &Style,
    link: Option<Ui>,
    ctx: &Ctx<'a>,
) {
    let p = ctx.app.theme.palette;
    let mut sp = span(s)
        .font(font_for(st))
        .size(st.size)
        .color(st.color)
        .underline(st.underline)
        .strikethrough(st.strike);
    if st.spoiler_hidden {
        sp = sp
            .color(p.spoiler)
            .background(p.spoiler)
            .link(Ui::RevealSpoiler(ctx.msg));
    } else if let Some(l) = link {
        sp = sp.link(l);
    }
    spans.push(sp);
}

fn inline<'a>(
    ctx: &Ctx<'a>,
    v: &[Inline],
    st: Style,
    link: Option<Ui>,
    spans: &mut Vec<Span<'a, Ui, Font>>,
) {
    let p = ctx.app.theme.palette;
    for i in v {
        match i {
            Inline::Text(t) => push_text(spans, t.clone(), &st, link.clone(), ctx),
            Inline::LineBreak => push_text(spans, "\n".into(), &st, None, ctx),
            Inline::Bold(c) => inline(ctx, c, Style { bold: true, ..st }, link.clone(), spans),
            Inline::Italic(c) => inline(ctx, c, Style { italic: true, ..st }, link.clone(), spans),
            Inline::Underline(c) => inline(
                ctx,
                c,
                Style {
                    underline: true,
                    ..st
                },
                link.clone(),
                spans,
            ),
            Inline::Strike(c) => inline(ctx, c, Style { strike: true, ..st }, link.clone(), spans),
            Inline::Spoiler(c) => {
                if ctx.revealed {
                    let start = spans.len();
                    inline(ctx, c, st, link.clone(), spans);
                    for sp in &mut spans[start..] {
                        let taken = std::mem::replace(sp, span(""));
                        *sp = taken.background(p.bg_hover);
                    }
                } else {
                    inline(
                        ctx,
                        c,
                        Style {
                            spoiler_hidden: true,
                            ..st
                        },
                        None,
                        spans,
                    );
                }
            }
            Inline::Code(code) => {
                let mut sp = span(code.clone())
                    .font(MONO)
                    .size(st.size * 0.85)
                    .color(if st.spoiler_hidden {
                        p.spoiler
                    } else {
                        p.text_normal
                    })
                    .background(if st.spoiler_hidden {
                        p.spoiler
                    } else {
                        p.bg_app
                    })
                    .border(Border {
                        radius: 3.0.into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    })
                    .padding(Padding::from([0, 2]));
                if st.spoiler_hidden {
                    sp = sp.link(Ui::RevealSpoiler(ctx.msg));
                }
                spans.push(sp);
            }
            Inline::Link { url, label } => {
                let lst = Style {
                    color: p.link,
                    ..st
                };
                inline(ctx, label, lst, Some(Ui::OpenUrl(url.clone())), spans);
            }
            Inline::UserMention(_)
            | Inline::RoleMention(_)
            | Inline::ChannelMention(_)
            | Inline::Everyone
            | Inline::Here => {
                let label = match i {
                    Inline::Everyone => "@everyone".to_string(),
                    Inline::Here => "@here".to_string(),
                    other => ctx.app.chat.resolve_mention(other).unwrap_or_default(),
                };
                let color = match i {
                    Inline::RoleMention(id) => ctx
                        .app
                        .chat
                        .role(*id)
                        .filter(|r| r.color != 0)
                        .map(|r| theme::from_int(r.color)),
                    _ => None,
                };
                let on = match i {
                    Inline::ChannelMention(id) => Some(Ui::SelectChannel(*id)),
                    _ => None,
                };
                if st.spoiler_hidden {
                    push_text(spans, label, &st, None, ctx);
                    continue;
                }
                let fg = color.unwrap_or(p.mention_text);
                let bg = color
                    .map(|c| Color { a: 0.15, ..c })
                    .unwrap_or(p.mention_bg);
                let mut sp = span(label)
                    .font(Font {
                        weight: font::Weight::Medium,
                        ..INTER
                    })
                    .size(st.size)
                    .color(fg)
                    .background(bg)
                    .border(Border {
                        radius: 3.0.into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    })
                    .padding(Padding::from([0, 2]));
                if let Some(on) = on {
                    sp = sp.link(on);
                }
                spans.push(sp);
            }
            Inline::Emoji { name, .. } => {
                let sp = span(format!(":{name}:"))
                    .font(font_for(&st))
                    .size(st.size)
                    .color(if st.spoiler_hidden {
                        p.spoiler
                    } else {
                        p.text_secondary
                    })
                    .background(if st.spoiler_hidden {
                        p.spoiler
                    } else {
                        p.bg_hover
                    })
                    .border(Border {
                        radius: 4.0.into(),
                        width: 0.0,
                        color: Color::TRANSPARENT,
                    })
                    .padding(Padding::from([0, 2]));
                spans.push(sp);
            }
            Inline::Timestamp { unix, style } => {
                let s = super::format_discord_timestamp(*unix, *style, crate::app::now_ms());
                spans.push(
                    span(s)
                        .font(INTER)
                        .size(st.size)
                        .color(p.text_normal)
                        .background(p.bg_hover)
                        .padding(Padding::from([0, 2])),
                );
            }
        }
    }
}

/// Emoji-only messages render large, like the official client.
fn jumbo<'a>(ctx: &Ctx<'a>, blocks: &[Block]) -> El<'a> {
    let p = ctx.app.theme.palette;
    let Some(Block::Paragraph(v)) = blocks.first() else {
        return column![].into();
    };
    let mut r = row![].spacing(4);
    for i in v {
        if let Inline::Emoji { name, id, .. } = i {
            let key = emoji_url(ctx.app, *id);
            let el: El<'a> = match ctx.app.image(&key) {
                Some(h) => iced::widget::image(h.clone()).width(48).height(48).into(),
                None => container(text(format!(":{name}:")).size(16).color(p.text_secondary))
                    .padding(Padding::from([10, 6]))
                    .style(theme::rounded_bg(p.bg_hover, 6.0))
                    .into(),
            };
            r = r.push(el);
        }
    }
    r.into()
}

pub fn emoji_url(app: &App, id: crate::net::model::Snowflake) -> String {
    format!(
        "{}/emojis/{id}.png?size=96",
        app.cfg.cdn_url.trim_end_matches('/')
    )
}
