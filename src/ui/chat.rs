//! Main pane: channel header, grouped message list with embeds and
//! attachments, composer and typing indicator.

use iced::widget::{
    Space, button, column, container, hover, mouse_area, progress_bar, rich_text, row, rule,
    scrollable, span, text, text_input,
};
use iced::{Alignment, Background, Border, Color, Font, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER, INTER_BOLD, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{
    El, avatar, fit_size, format_day, format_time, format_timestamp, human_size, markdown,
    same_day, ui,
};
use crate::app::actions::{composer_id, edit_id, search_id};
use crate::app::emoji::PickerTarget;
use crate::app::state::continues_group;
use crate::app::{App, Message, Ui, View, emoji_url, jump, now_ms, preview_key};
use crate::net::model::{self as model, Attachment, Channel, ChannelKind, Embed};

pub fn view(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let channel = app.open_channel().and_then(|c| app.chat.channels.get(&c));
    let body: El<'_> = match channel {
        Some(c) => column![
            header(app, c),
            messages(app, c),
            detached_bar(app, c),
            composer(app, c)
        ]
        .into(),
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
        View::Home(_) => "Pick a conversation on the left to start chatting.",
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

/// Where the header's help button leads (Discord's Help Center, as in the
/// official client).
const HELP_URL: &str = "https://support.discord.com";

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
    let in_guild = matches!(app.view, View::Guild { .. });
    let members_icon = icon(
        Icon::Users,
        24.0,
        if app.members_open && in_guild {
            p.header_text
        } else {
            p.text_secondary
        },
    );
    let members: El<'a> = if in_guild {
        button(members_icon)
            .padding(0)
            .on_press(ui(Ui::ToggleMembers))
            .style(theme::link_button(p))
            .into()
    } else {
        members_icon
    };
    let searching = app.search.open || app.search.panel_visible();
    let search_input = text_input("Search", &app.search.input)
        .id(search_id())
        .on_input(|s| Message::Ui(Ui::SearchChanged(s)))
        .on_submit(Message::Ui(Ui::SubmitSearch))
        .size(14)
        .font(INTER)
        .padding(Padding::from([2, 6]))
        .style(theme::input(p, p.bg_app));
    let search_end: El<'a> = if searching {
        button(icon(Icon::X, 16.0, p.text_muted))
            .padding(0)
            .on_press(ui(Ui::CloseSearch))
            .style(theme::link_button(p))
            .into()
    } else {
        icon(Icon::Search, 16.0, p.text_muted)
    };
    let search = container(
        row![search_input, search_end]
            .spacing(4)
            .align_y(Alignment::Center),
    )
    .width(if searching { 240 } else { 144 })
    .height(28)
    .padding(Padding::from([0, 6]))
    .align_y(Alignment::Center)
    .style(theme::rounded_bg(p.bg_app, 4.0));
    let tools = row![
        tool(Icon::Bell),
        tool(Icon::Pin),
        members,
        search,
        tool(Icon::Inbox),
        button(tool(Icon::CircleHelp))
            .padding(0)
            .on_press(ui(Ui::OpenUrl(HELP_URL.to_owned())))
            .style(theme::link_button(p))
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
        .id(jump::list_id())
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
    let editing = app.editing.as_ref().filter(|e| e.message == m.id);
    let mut body = column![].spacing(4).width(Length::Fill);
    if let Some(e) = editing {
        body = body.push(edit_box(app, e));
    } else if !m.content.is_empty() {
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
    if !m.reactions.is_empty() {
        body = body.push(reactions_row(app, c, m));
    }
    if m.failed {
        body = body.push(
            text("Message could not be delivered.")
                .size(12)
                .color(p.danger),
        );
    }

    let gutter = 72.0;
    let preview = if m.referenced.is_some() {
        app.chat.reply_preview(m)
    } else {
        None
    };
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
        let main = row![
            container(avatar(app, &m.author, 40.0))
                .width(gutter)
                .padding(Padding::ZERO.left(16).top(2)),
            column![name_row, body].spacing(2).width(Length::Fill)
        ]
        .padding(Padding::from([2, 0]).right(48));
        match preview {
            Some(pv) => column![
                container(reply_header(app, c, m, &pv)).padding(Padding::ZERO.top(16)),
                main
            ]
            .into(),
            None => container(main).padding(Padding::ZERO.top(16)).into(),
        }
    };

    let jump_target = app.highlight == Some(m.id);
    let menu_target = app.menu.is_some_and(|mn| mn.message == m.id);
    let tint = if jump_target {
        Some(Color { a: 0.12, ..p.link })
    } else if mentioned {
        Some(p.bubble_in)
    } else if menu_target {
        Some(p.bg_hover)
    } else {
        None
    };
    let bar: El<'a> = if mentioned {
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
    };
    let base_el: El<'a> = match tint {
        Some(t) => container(bar)
            .width(Length::Fill)
            .style(theme::bg(t))
            .into(),
        None => container(bar).width(Length::Fill).into(),
    };
    let interactive = !m.pending && !m.failed && matches!(m.kind, 0 | 19) && editing.is_none();
    let el: El<'a> = if interactive {
        let overlay = container(action_bar(app, c, m))
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(Padding::ZERO.right(16))
            .align_right(Length::Fill)
            .align_top(Length::Fill)
            .style(theme::bg(Color {
                a: 0.04,
                ..p.text_primary
            }));
        mouse_area(hover(base_el, overlay))
            .on_right_press(ui(Ui::ContextMenu(m.id)))
            .into()
    } else {
        base_el
    };
    if jump_target {
        container(el).id(jump::row_id(m.id)).into()
    } else {
        el
    }
}

/// "╭─ (avatar) @name snippet": click jumps to the original message.
fn reply_header<'a>(
    app: &'a App,
    c: &'a Channel,
    m: &'a model::Message,
    pv: &model::ReplyPreview,
) -> El<'a> {
    let p = app.theme.palette;
    let spine = container(icon(Icon::ReplySpine, 24.0, p.text_muted))
        .width(Length::Shrink)
        .padding(Padding::ZERO.left(36).top(6));
    let mut r = row![spine].spacing(4).align_y(Alignment::Center);
    let target = pv.message_id.or(m.reply_to);
    let content: El<'a> = if pv.deleted {
        text("Original message was deleted")
            .size(14)
            .font(theme::INTER_ITALIC)
            .color(p.text_muted)
            .into()
    } else {
        let mut inner = row![].spacing(4).align_y(Alignment::Center);
        if let Some(a) = &pv.author {
            let color = app
                .chat
                .author_color(
                    m.guild_id.or(c.guild_id),
                    &model::Message {
                        author: a.clone(),
                        author_roles: vec![],
                        ..m.clone()
                    },
                )
                .map(theme::from_int)
                .unwrap_or(p.text_secondary);
            inner = inner.push(avatar(app, a, 16.0)).push(
                text(format!("@{}", a.display_name()))
                    .size(14)
                    .font(INTER_MEDIUM)
                    .color(color)
                    .wrapping(text::Wrapping::None),
            );
        }
        let snippet = pv.content.replace('\n', " ");
        let snippet = if snippet.is_empty() && pv.has_attachment {
            "Click to see attachment".to_owned()
        } else {
            snippet
        };
        inner = inner.push(
            text(snippet)
                .size(14)
                .color(p.text_secondary)
                .wrapping(text::Wrapping::None),
        );
        if pv.has_attachment {
            inner = inner.push(icon(Icon::Image, 16.0, p.text_secondary));
        }
        inner.into()
    };
    let pressable = container(content).max_width(640).clip(true);
    r = r.push(match target {
        Some(t) if !pv.deleted => button(pressable)
            .padding(0)
            .on_press(ui(Ui::JumpTo {
                channel: m.channel_id,
                message: t,
            }))
            .style(theme::link_button(p))
            .into(),
        _ => El::from(pressable),
    });
    r.into()
}

fn reactions_row<'a>(app: &'a App, c: &'a Channel, m: &'a model::Message) -> El<'a> {
    let p = app.theme.palette;
    let mut pills: Vec<El<'a>> = Vec::new();
    for re in &m.reactions {
        let face: El<'a> = match re.emoji.id {
            Some(id) => match app.image(&emoji_url(&app.cfg.cdn_url, id)) {
                Some(h) => iced::widget::image(h.clone()).width(18).height(18).into(),
                None => text(format!(":{}:", re.emoji.name))
                    .size(12)
                    .color(p.text_muted)
                    .into(),
            },
            None => text(re.emoji.name.clone()).size(16).into(),
        };
        let pill = button(
            row![
                face,
                text(re.count.to_string())
                    .size(14)
                    .font(INTER_MEDIUM)
                    .color(if re.me { p.link } else { p.text_secondary }),
            ]
            .spacing(6)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([2, 6]))
        .on_press(ui(Ui::ToggleReaction(m.id, re.emoji.clone())))
        .style(theme::reaction_pill(p, re.me));
        pills.push(pill.into());
    }
    let _ = c;
    row(pills)
        .spacing(4)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(4)
        .into()
}

/// Floating toolbar shown while hovering a message.
fn action_bar<'a>(app: &'a App, c: &'a Channel, m: &'a model::Message) -> El<'a> {
    let p = app.theme.palette;
    let mine = Some(m.author.id) == app.chat.me_id();
    let tool = |i: Icon, msg: Ui| -> El<'a> {
        button(container(icon(i, 18.0, p.text_secondary)).center(28))
            .padding(0)
            .on_press(ui(msg))
            .style(theme::icon_button(p))
            .into()
    };
    let mut r = row![tool(
        Icon::SmilePlus,
        Ui::OpenEmojiPicker(PickerTarget::Reaction {
            channel: c.id,
            message: m.id
        })
    )]
    .spacing(0);
    if mine && matches!(m.kind, 0 | 19) {
        r = r.push(tool(Icon::Pencil, Ui::StartEdit(m.id)));
    }
    r = r
        .push(tool(Icon::Reply, Ui::StartReply(m.id)))
        .push(tool(Icon::Ellipsis, Ui::ContextMenu(m.id)));
    container(r)
        .padding(2)
        .style(move |_| container::Style {
            background: Some(Background::Color(p.bg_panel)),
            border: Border {
                radius: 6.0.into(),
                width: 1.0,
                color: p.border,
            },
            ..Default::default()
        })
        .into()
}

/// Inline editor shown in place of the message text.
fn edit_box<'a>(app: &'a App, e: &'a crate::app::actions::Editing) -> El<'a> {
    let p = app.theme.palette;
    let input = text_input("Edit message", &e.text)
        .id(edit_id())
        .on_input(|s| Message::Ui(Ui::EditChanged(s)))
        .on_submit(Message::Ui(Ui::SubmitEdit))
        .size(16)
        .font(INTER)
        .padding(Padding::from([10, 12]))
        .style(theme::input(p, p.composer_bg));
    let link = |label: &'static str, msg: Ui| -> El<'a> {
        button(text(label).size(12).color(p.link))
            .padding(0)
            .on_press(ui(msg))
            .style(theme::link_button(p))
            .into()
    };
    column![
        input,
        row![
            text("escape to ").size(12).color(p.text_muted),
            link("cancel", Ui::CancelEdit),
            text(" \u{2022} enter to ").size(12).color(p.text_muted),
            link("save", Ui::SubmitEdit),
        ]
    ]
    .spacing(4)
    .into()
}

fn detached_bar<'a>(app: &'a App, c: &Channel) -> El<'a> {
    let p = app.theme.palette;
    let detached = app.chat.messages.get(&c.id).is_some_and(|w| w.detached);
    if !detached {
        return Space::new().into();
    }
    container(
        row![
            text("You're viewing older messages")
                .size(14)
                .font(INTER_MEDIUM)
                .color(p.accent_text)
                .width(Length::Fill),
            button(
                row![
                    text("Jump To Present")
                        .size(14)
                        .font(INTER_SEMIBOLD)
                        .color(p.accent_text),
                    icon(Icon::ArrowDown, 16.0, p.accent_text),
                ]
                .spacing(4)
                .align_y(Alignment::Center)
            )
            .padding(0)
            .on_press(ui(Ui::JumpToPresent))
            .style(theme::link_button(p)),
        ]
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 16]))
    .width(Length::Fill)
    .style(theme::bg(p.accent))
    .into()
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
            // The title is arbitrary text: confirm the real target.
            sp = sp.link(Ui::ConfirmLink(u.clone()));
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
    if let (Some(th), None) = (&e.thumbnail, &e.image)
        && let Some(handle) = app.image(&th.url)
    {
        inner = inner.push(
            iced::widget::image(handle.clone())
                .width(80)
                .height(80)
                .border_radius(4.0),
        );
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
    let tool = |i: Icon, msg: Option<Ui>| -> El<'a> {
        match msg {
            Some(m) => button(icon(i, 24.0, p.text_secondary))
                .padding(0)
                .on_press(ui(m))
                .style(theme::link_button(p))
                .into(),
            None => icon(i, 24.0, p.text_secondary),
        }
    };
    let input = text_input(&placeholder, &app.composer)
        .id(composer_id())
        .on_input(|s| Message::Ui(Ui::ComposerChanged(s)))
        .on_submit(Message::Ui(Ui::Send))
        .size(16)
        .font(INTER)
        .padding(Padding::from([11, 0]))
        .style(theme::input(p, p.composer_bg));
    let bar = container(
        row![
            tool(Icon::CirclePlus, Some(Ui::PickFiles)),
            input,
            tool(Icon::Gift, None),
            tool(Icon::Sticker, None),
            tool(
                Icon::Smile,
                Some(Ui::OpenEmojiPicker(PickerTarget::Composer))
            )
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([0, 16]))
    .style(theme::rounded_bg(p.composer_bg, 8.0));

    let mut col = column![].padding(Padding::from([0, 16]));
    if let Some(r) = reply_bar(app, c) {
        col = col.push(r);
    }
    if !app.draft.is_empty() {
        col = col.push(draft_strip(app));
    }
    for u in app.uploads.iter().filter(|u| u.channel == c.id) {
        col = col.push(upload_strip(app, u));
    }
    col = col.push(bar);
    column![col, typing(app, c)].into()
}

/// "Replying to Name   @ ON  x" bar attached to the top of the composer.
fn reply_bar<'a>(app: &'a App, c: &Channel) -> Option<El<'a>> {
    let p = app.theme.palette;
    let r = app.reply.as_ref().filter(|r| r.channel == c.id)?;
    let mine = Some(r.author.id) == app.chat.me_id();
    let mut row_ = row![
        text("Replying to ").size(14).color(p.text_secondary),
        text(r.author.display_name().to_owned())
            .size(14)
            .font(INTER_SEMIBOLD)
            .color(p.text_primary)
            .width(Length::Fill),
    ]
    .align_y(Alignment::Center)
    .spacing(0);
    if !mine {
        // "@ ON" / "@ OFF": whether the reply pings the author.
        let label = if r.mention { "ON" } else { "OFF" };
        let toggle = button(
            row![
                text("@").size(14).font(INTER_BOLD).color(if r.mention {
                    p.accent
                } else {
                    p.text_muted
                }),
                text(label).size(12).font(INTER_BOLD).color(if r.mention {
                    p.accent
                } else {
                    p.text_muted
                }),
            ]
            .spacing(3)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([2, 6]))
        .on_press(ui(Ui::ToggleReplyMention))
        .style(theme::icon_button(p));
        row_ = row_.push(toggle);
    }
    row_ = row_.push(
        button(icon(Icon::X, 18.0, p.text_secondary))
            .padding(Padding::from([0, 4]))
            .on_press(ui(Ui::CancelReply))
            .style(theme::icon_button(p)),
    );
    Some(
        container(row_)
            .padding(Padding::from([8, 12]))
            .width(Length::Fill)
            .style(move |_| container::Style {
                background: Some(Background::Color(p.bg_sidebar)),
                border: Border {
                    radius: iced::border::Radius::default().top(8.0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into(),
    )
}

/// Files waiting to be sent: thumbnail or file icon, name, size, remove.
fn draft_strip<'a>(app: &'a App) -> El<'a> {
    let p = app.theme.palette;
    let mut r = row![].spacing(12);
    for (i, a) in app.draft.iter().enumerate() {
        let thumb: El<'a> = match app.image(&a.key()) {
            Some(h) if a.is_image => iced::widget::image(h.clone())
                .width(96)
                .height(96)
                .border_radius(4.0)
                .into(),
            _ => container(icon(
                if a.is_image { Icon::Image } else { Icon::File },
                40.0,
                p.accent,
            ))
            .center(96)
            .into(),
        };
        let remove = button(icon(Icon::Trash, 16.0, p.danger))
            .padding(4)
            .on_press(ui(Ui::RemoveAttachment(i)))
            .style(theme::icon_button(p));
        r = r.push(
            container(
                column![
                    container(thumb).center_x(Length::Fill),
                    row![
                        text(a.file.name.clone())
                            .size(12)
                            .color(p.text_normal)
                            .wrapping(text::Wrapping::None)
                            .width(Length::Fill),
                        remove,
                    ]
                    .align_y(Alignment::Center),
                    text(human_size(a.file.size)).size(11).color(p.text_muted),
                ]
                .spacing(2),
            )
            .width(128)
            .padding(8)
            .clip(true)
            .style(theme::rounded_bg(p.bg_sidebar, 6.0)),
        );
    }
    container(scrollable(r).direction(scrollable::Direction::Horizontal(
        scrollable::Scrollbar::new().width(4).scroller_width(4),
    )))
    .padding(Padding::from([8, 12]))
    .width(Length::Fill)
    .style(move |_| container::Style {
        background: Some(Background::Color(p.bg_sidebar)),
        border: Border {
            radius: iced::border::Radius::default().top(8.0),
            ..Default::default()
        },
        ..Default::default()
    })
    .into()
}

fn upload_strip<'a>(app: &'a App, u: &'a crate::app::upload::ActiveUpload) -> El<'a> {
    let p = app.theme.palette;
    container(
        row![
            icon(Icon::File, 20.0, p.accent),
            column![
                text(format!("Uploading {}", u.label))
                    .size(13)
                    .color(p.text_normal)
                    .wrapping(text::Wrapping::None),
                progress_bar(0.0..=1.0, u.fraction())
                    .girth(4)
                    .style(move |_| progress_bar::Style {
                        background: Background::Color(p.bg_app),
                        bar: Background::Color(p.accent),
                        border: Border {
                            radius: 2.0.into(),
                            ..Default::default()
                        },
                    }),
            ]
            .spacing(4)
            .width(Length::Fill),
            text(format!("{:.0}%", u.fraction() * 100.0))
                .size(12)
                .color(p.text_muted),
            button(icon(Icon::X, 16.0, p.text_secondary))
                .padding(4)
                .on_press(ui(Ui::CancelUpload(u.id)))
                .style(theme::icon_button(p)),
        ]
        .spacing(10)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([8, 12]))
    .width(Length::Fill)
    .style(theme::bg(p.bg_sidebar))
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
