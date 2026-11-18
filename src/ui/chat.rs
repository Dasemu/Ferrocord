//! Main pane: channel header, grouped message list with embeds and
//! attachments, composer and typing indicator.

use iced::widget::{
    Space, button, column, container, rich_text, row, rule, scrollable, span, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Font, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER, INTER_BOLD, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{
    El, avatar, fit_size, format_day, format_time, format_timestamp, human_size, markdown,
    same_day, ui,
};
use crate::app::state::continues_group;
use crate::app::{App, Message, Ui, View, now_ms, preview_key};
use crate::net::model::{self as model, Attachment, Channel, ChannelKind, Embed};

pub fn view(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let channel = app.open_channel().and_then(|c| app.chat.channels.get(&c));
    let body: El<'_> = match channel {
        Some(c) => column![header(app, c), messages(app, c), composer(app, c)].into(),
        None => empty(app),
    };
    container(body)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::bg(p.bg_panel))
        .into()
}

fn empty(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let msg = match app.view {
        View::Home(_) => "No one's around to play with Wumpus… pick a conversation on the left.",
        View::Guild { .. } => "This server has no text channels you can see.",
    };
    container(
        column![
            icon(Icon::Users, 64.0, p.text_muted),
            text(msg).size(16).color(p.text_muted)
        ]
        .spacing(16)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

fn channel_title(app: &App, c: &Channel) -> String {
    match c.kind {
        ChannelKind::Dm | ChannelKind::GroupDm => c.label(|u| app.chat.user_name(u)),
        _ => c.name.clone().unwrap_or_default(),
    }
}

fn header<'a>(app: &'a App, c: &'a Channel) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let lead = match c.kind {
        ChannelKind::Dm => Icon::AtSign,
        ChannelKind::GroupDm => Icon::Users,
        ChannelKind::Announcement => Icon::Megaphone,
        _ => Icon::Hash,
    };
    let mut r = row![
        icon(lead, 24.0, p.channel_icon),
        text(channel_title(app, c))
            .size(16)
            .font(INTER_SEMIBOLD)
            .color(p.header_text)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if let Some(topic) = c.topic.as_deref().filter(|t| !t.is_empty()) {
        r = r
            .push(
                container(rule::vertical(1).style(move |_| rule::Style {
                    color: p.bg_selected,
                    radius: 0.0.into(),
                    fill_mode: rule::FillMode::Full,
                    snap: true,
                }))
                .height(24)
                .padding(Padding::from([0, 4])),
            )
            .push(
                text(topic.to_owned())
                    .size(14)
                    .color(p.text_secondary)
                    .wrapping(text::Wrapping::None),
            );
    }
    let tool = |i: Icon| icon(i, 24.0, p.text_secondary);
    let search = container(
        row![
            text("Search")
                .size(14)
                .color(p.text_muted)
                .width(Length::Fill),
            icon(Icon::Search, 16.0, p.text_muted)
        ]
        .align_y(Alignment::Center),
    )
    .width(144)
    .height(24)
    .padding(Padding::from([0, 6]))
    .align_y(Alignment::Center)
    .style(theme::rounded_bg(p.bg_app, 4.0));
    let tools = row![
        tool(Icon::Bell),
        tool(Icon::Pin),
        tool(Icon::Users),
        search,
        tool(Icon::Inbox),
        tool(Icon::CircleHelp)
    ]
    .spacing(16)
    .align_y(Alignment::Center);
    container(
        row![container(r).width(Length::Fill).clip(true), tools]
            .spacing(16)
            .align_y(Alignment::Center),
    )
    .height(t.metrics.header_height)
    .width(Length::Fill)
    .padding(Padding::from([0, 16]))
    .align_y(Alignment::Center)
    .style(theme::header(p))
    .into()
}

fn welcome<'a>(app: &'a App, c: &'a Channel) -> El<'a> {
    let p = app.theme.palette;
    let (title, sub) = match c.kind {
        ChannelKind::Dm | ChannelKind::GroupDm => (
            channel_title(app, c),
            format!(
                "This is the beginning of your direct message history with {}.",
                channel_title(app, c)
            ),
        ),
        _ => (
            format!("Welcome to #{}!", c.name.as_deref().unwrap_or("")),
            format!(
                "This is the start of the #{} channel.",
                c.name.as_deref().unwrap_or("")
            ),
        ),
    };
    let badge: El<'a> = match c.kind {
        ChannelKind::Dm => match c.recipients.first().and_then(|u| app.chat.users.get(u)) {
            Some(u) => avatar(app, u, 80.0),
            None => Space::new().into(),
        },
        _ => container(icon(Icon::Hash, 42.0, p.header_text))
            .center(68)
            .style(theme::rounded_bg(p.bg_selected, 34.0))
            .into(),
    };
    column![
        badge,
        text(title).size(32).font(INTER_BOLD).color(p.header_text),
        text(sub).size(16).color(p.text_secondary),
    ]
    .spacing(8)
    .padding(Padding::from([16, 16]).bottom(8))
    .into()
}

fn day_divider<'a>(app: &App, ms: u64) -> El<'a> {
    let p = app.theme.palette;
    let line = move || {
        rule::horizontal(1).style(move |_| rule::Style {
            color: p.bg_selected,
            radius: 0.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        })
    };
    row![
        line(),
        text(format_day(ms))
            .size(12)
            .font(INTER_SEMIBOLD)
            .color(p.text_muted),
        line()
    ]
    .spacing(8)
    .padding(Padding::from([8, 16]))
    .align_y(Alignment::Center)
    .into()
}

fn messages<'a>(app: &'a App, c: &'a Channel) -> El<'a> {
    let p = app.theme.palette;
    let now = now_ms();
    let window = app.chat.messages.get(&c.id);
    let mut col = column![].width(Length::Fill);
    match window {
        Some(w) if w.reached_start => col = col.push(welcome(app, c)),
        Some(w) if w.loading => {
            col = col.push(
                container(text("Loading messages…").size(14).color(p.text_muted))
                    .padding(16)
                    .center_x(Length::Fill),
            );
        }
        _ => col = col.push(welcome(app, c)),
    }
    if let Some(w) = window {
        let mut prev: Option<&model::Message> = None;
        for m in &w.messages {
            let new_day = prev.is_none_or(|pm| !same_day(pm.timestamp_ms(), m.timestamp_ms()));
            if new_day && prev.is_some() {
                col = col.push(day_divider(app, m.timestamp_ms()));
            }
            let grouped = !new_day && prev.is_some_and(|pm| continues_group(pm, m));
            col = col.push(message(app, c, m, grouped, now));
            prev = Some(m);
        }
    }
    col = col.push(Space::new().height(24));
    // `direction` resets the anchor, so it must come first.
    scrollable(col)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(8)
                .scroller_width(8)
                .margin(2),
        ))
        .anchor_bottom()
        .on_scroll(|vp| Message::Ui(Ui::Scrolled(vp)))
        .style(theme::thin_scrollbar(p, true))
        .height(Length::Fill)
        .into()
}

fn message<'a>(
    app: &'a App,
    c: &'a Channel,
    m: &'a model::Message,
    grouped: bool,
    now: u64,
) -> El<'a> {
    let p = app.theme.palette;
    let mentioned = app.chat.mentions_me(m) && !c.kind.is_private();
    let base = if m.failed {
        p.danger
    } else if m.pending {
        p.text_muted
    } else {
        p.text_normal
    };
    let mut body = column![].spacing(4).width(Length::Fill);
    if !m.content.is_empty() {
        let content = markdown::render(app, m.id, &m.content, base, 16.0);
        if m.edited {
            body = body.push(column![
                content,
                text("(edited)").size(10).color(p.text_muted)
            ]);
        } else {
            body = body.push(content);
        }
    }
    for a in &m.attachments {
        body = body.push(attachment(app, a));
    }
    for e in &m.embeds {
        body = body.push(embed(app, m, e));
    }
    if m.failed {
        body = body.push(
            text("Message could not be delivered.")
                .size(12)
                .color(p.danger),
        );
    }

    let gutter = 72.0;
    let line: El<'a> = if grouped {
        row![
            container(
                text(format_time(m.timestamp_ms()))
                    .size(11)
                    .color(Color::TRANSPARENT)
            )
            .width(gutter)
            .padding(Padding::ZERO.left(16).top(4)),
            body
        ]
        .padding(Padding::from([2, 0]).right(48))
        .into()
    } else {
        let name_color = app
            .chat
            .author_color(m.guild_id.or(c.guild_id), m)
            .map(theme::from_int)
            .unwrap_or(p.header_text);
        let mut name_row = row![
            text(m.author.display_name().to_owned())
                .size(16)
                .font(INTER_MEDIUM)
                .color(name_color),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        if m.author.bot {
            name_row = name_row.push(
                container(
                    text("BOT")
                        .size(10)
                        .font(INTER_SEMIBOLD)
                        .color(p.accent_text),
                )
                .padding(Padding::from([1, 4]))
                .style(theme::rounded_bg(p.accent, 3.0)),
            );
        }
        name_row = name_row.push(
            text(format_timestamp(m.timestamp_ms(), now))
                .size(12)
                .color(p.text_muted),
        );
        row![
            container(avatar(app, &m.author, 40.0))
                .width(gutter)
                .padding(Padding::ZERO.left(16).top(2)),
            column![name_row, body].spacing(2).width(Length::Fill)
        ]
        .padding(Padding::from([2, 0]).top(16).right(48))
        .into()
    };
    if mentioned {
        row![
            container(text(""))
                .width(2)
                .height(Length::Fill)
                .style(theme::bg(p.mention_bar)),
            container(line)
                .width(Length::Fill)
                .padding(Padding::ZERO.bottom(2)),
        ]
        .height(Length::Shrink)
        .into()
    } else {
        line
    }
    .pipe(|el| {
        if mentioned {
            container(el)
                .width(Length::Fill)
                .style(theme::bg(p.bubble_in))
                .into()
        } else {
            el
        }
    })
}

trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}
impl<T> Pipe for T {}

fn attachment<'a>(app: &'a App, a: &'a Attachment) -> El<'a> {
    let p = app.theme.palette;
    if a.is_image() {
        let (w, h) = fit_size(a.width.unwrap_or(400), a.height.unwrap_or(300), 400, 300);
        return match app.image(&preview_key(a)) {
            Some(handle) => iced::widget::image(handle.clone())
                .width(w as f32)
                .height(h as f32)
                .border_radius(8.0)
                .into(),
            None => container(icon(Icon::Image, 32.0, p.text_muted))
                .width(w as f32)
                .height(h as f32)
                .center_x(w as f32)
                .center_y(h as f32)
                .style(theme::rounded_bg(p.embed_bg, 8.0))
                .into(),
        };
    }
    let link_spans = vec![
        span::<Ui, Font>(a.filename.clone())
            .color(p.link)
            .font(INTER)
            .link(Ui::OpenUrl(a.url.clone())),
    ];
    container(
        row![
            icon(Icon::File, 30.0, p.accent),
            column![
                rich_text(link_spans).size(16).on_link_click(Message::Ui),
                text(human_size(a.size)).size(12).color(p.text_muted),
            ]
            .spacing(2)
            .width(Length::Fill),
            button(icon(Icon::Download, 24.0, p.text_secondary))
                .padding(0)
                .on_press(ui(Ui::OpenUrl(a.url.clone())))
                .style(theme::link_button(p)),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .width(432)
    .padding(16)
    .style(move |_| container::Style {
        background: Some(Background::Color(p.embed_bg)),
        border: Border {
            radius: 8.0.into(),
            width: 1.0,
            color: p.border,
        },
        ..Default::default()
    })
    .into()
}

fn embed<'a>(app: &'a App, m: &'a model::Message, e: &'a Embed) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    let mut col = column![].spacing(8).width(Length::Fill);
    if let Some(pn) = &e.provider_name {
        col = col.push(text(pn.clone()).size(12).color(p.text_secondary));
    }
    if let Some(an) = &e.author_name {
        col = col.push(
            text(an.clone())
                .size(14)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
        );
    }
    if let Some(title) = &e.title {
        let mut sp = span::<Ui, Font>(title.clone())
            .font(INTER_SEMIBOLD)
            .size(16)
            .color(if e.url.is_some() {
                p.link
            } else {
                p.header_text
            });
        if let Some(u) = &e.url {
            sp = sp.link(Ui::OpenUrl(u.clone()));
        }
        col = col.push(rich_text(vec![sp]).on_link_click(Message::Ui));
    }
    if let Some(d) = &e.description {
        col = col.push(markdown::render(app, m.id, d, p.text_normal, 14.0));
    }
    if !e.fields.is_empty() {
        let mut fields = column![].spacing(8);
        let mut line = row![].spacing(8);
        let mut n = 0;
        for f in &e.fields {
            let cell = column![
                text(f.name.clone())
                    .size(14)
                    .font(INTER_SEMIBOLD)
                    .color(p.header_text),
                markdown::render(app, m.id, &f.value, p.text_normal, 14.0),
            ]
            .spacing(2)
            .width(Length::Fill);
            if !f.inline || n == 3 {
                if n > 0 {
                    fields = fields.push(line);
                }
                line = row![].spacing(8);
                n = 0;
            }
            line = line.push(cell);
            n += 1;
            if !f.inline {
                fields = fields.push(line);
                line = row![].spacing(8);
                n = 0;
            }
        }
        if n > 0 {
            fields = fields.push(line);
        }
        col = col.push(fields);
    }
    if let Some(img) = &e.image {
        let (w, h) = fit_size(
            img.width.unwrap_or(400),
            img.height.unwrap_or(300),
            400,
            300,
        );
        if let Some(handle) = app.image(&img.url) {
            col = col.push(
                iced::widget::image(handle.clone())
                    .width(w as f32)
                    .height(h as f32)
                    .border_radius(4.0),
            );
        }
    }
    if let Some(f) = &e.footer {
        col = col.push(
            text(f.clone())
                .size(12)
                .font(INTER_MEDIUM)
                .color(p.text_secondary),
        );
    }
    let mut inner = row![col].spacing(16);
    if let (Some(th), None) = (&e.thumbnail, &e.image) {
        if let Some(handle) = app.image(&th.url) {
            inner = inner.push(
                iced::widget::image(handle.clone())
                    .width(80)
                    .height(80)
                    .border_radius(4.0),
            );
        }
    }
    let bar = e.color.map(theme::from_int).unwrap_or(p.bg_app);
    container(
        container(inner)
            .padding(Padding::from([8, 16]).top(8).bottom(16).left(12))
            .width(Length::Fill),
    )
    .max_width(t.metrics.embed_max_width)
    .style(move |_| container::Style {
        background: Some(Background::Color(p.embed_bg)),
        border: Border {
            radius: 4.0.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        ..Default::default()
    })
    .pipe(|c| {
        row![
            container(text(""))
                .width(4)
                .height(Length::Fill)
                .style(move |_| container::Style {
                    background: Some(Background::Color(bar)),
                    border: Border {
                        radius: iced::border::Radius::default().left(4.0),
                        ..Default::default()
                    },
                    ..Default::default()
                }),
            c
        ]
        .height(Length::Shrink)
    })
    .pipe(|r| container(r).max_width(t.metrics.embed_max_width + 4.0))
    .into()
}

fn composer<'a>(app: &'a App, c: &'a Channel) -> El<'a> {
    let p = app.theme.palette;
    let placeholder = match c.kind {
        ChannelKind::Dm | ChannelKind::GroupDm => format!("Message @{}", channel_title(app, c)),
        _ => format!("Message #{}", c.name.as_deref().unwrap_or("")),
    };
    let tool = |i: Icon| icon(i, 24.0, p.text_secondary);
    let input = text_input(&placeholder, &app.composer)
        .on_input(|s| Message::Ui(Ui::ComposerChanged(s)))
        .on_submit(Message::Ui(Ui::Send))
        .size(16)
        .font(INTER)
        .padding(Padding::from([11, 0]))
        .style(theme::input(p, p.composer_bg));
    let bar = container(
        row![
            tool(Icon::CirclePlus),
            input,
            tool(Icon::Gift),
            tool(Icon::Sticker),
            tool(Icon::Smile)
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([0, 16]))
    .style(theme::rounded_bg(p.composer_bg, 8.0));

    column![
        container(bar).padding(Padding::from([0, 16])),
        typing(app, c),
    ]
    .into()
}

fn typing<'a>(app: &'a App, c: &'a Channel) -> El<'a> {
    let p = app.theme.palette;
    let names = app.chat.typing_names(c.id, now_ms());
    let mut spans: Vec<iced::widget::text::Span<'a, Ui, Font>> = Vec::new();
    let bold = |s: String| span(s).font(INTER_BOLD).color(p.text_normal).size(12);
    let reg = |s: &str| span(s.to_owned()).font(INTER).color(p.text_normal).size(12);
    match names.len() {
        0 => {}
        1 => {
            spans.push(bold(names[0].clone()));
            spans.push(reg(" is typing…"));
        }
        2 => {
            spans.push(bold(names[0].clone()));
            spans.push(reg(" and "));
            spans.push(bold(names[1].clone()));
            spans.push(reg(" are typing…"));
        }
        3 => {
            spans.push(bold(names[0].clone()));
            spans.push(reg(", "));
            spans.push(bold(names[1].clone()));
            spans.push(reg(" and "));
            spans.push(bold(names[2].clone()));
            spans.push(reg(" are typing…"));
        }
        _ => spans.push(reg("Several people are typing…")),
    }
    let content: El<'a> = if spans.is_empty() {
        Space::new().into()
    } else {
        row![
            text("•••").size(12).font(INTER_BOLD).color(p.text_normal),
            rich_text(spans).size(12)
        ]
        .spacing(6)
        .align_y(Alignment::Center)
        .into()
    };
    container(content)
        .height(24)
        .padding(Padding::from([0, 16]).left(24))
        .align_y(Alignment::Center)
        .into()
}
