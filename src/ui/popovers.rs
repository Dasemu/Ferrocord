//! Popovers under the channel header: notification settings (bell), pinned
//! messages and the mentions inbox.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length, Padding, Point, Size};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar, format_timestamp, ui};
use crate::app::panels::Popover;
use crate::app::{App, Ui, View, input, now_ms};
use crate::net::extras::{MuteDuration, NotifyLevel};
use crate::net::model::{self as model, Message as Msg, truncate_chars};

pub const BELL_W: f32 = 300.0;
pub const PINS_W: f32 = 420.0;
pub const INBOX_W: f32 = 440.0;
const MAX_H: f32 = 520.0;

/// Width of the right-hand panel (member list or search results) that the
/// popovers must stay clear of.
fn right_panel_width(app: &App) -> f32 {
    if app.search.panel_visible() {
        super::search::WIDTH
    } else if app.members_open
        && matches!(
            app.view,
            View::Guild {
                channel: Some(_),
                ..
            }
        )
    {
        super::members::WIDTH
    } else {
        0.0
    }
}

/// Top-left corner of the popover for `kind`: under the header, right
/// aligned with the header buttons.
pub fn position(app: &App, size: Size) -> Point {
    let window = input::window_size();
    let x = window.width - right_panel_width(app) - size.width - 12.0;
    let y = app.theme.metrics.header_height + 4.0;
    input::clamp_popup(Point::new(x, y), size, window)
}

pub fn size_of(app: &App, kind: Popover) -> Size {
    match kind {
        Popover::Bell => Size::new(BELL_W, bell_height(app)),
        Popover::Pins => Size::new(PINS_W, MAX_H),
        Popover::Inbox => Size::new(INBOX_W, MAX_H),
    }
}

fn bell_height(app: &App) -> f32 {
    let rows = if app
        .open_channel()
        .is_some_and(|c| app.chat.is_channel_muted(c))
    {
        1.0
    } else {
        MuteDuration::ALL.len() as f32
    };
    // Title + two section labels + rows + paddings.
    48.0 + 2.0 * 28.0 + (rows + 4.0) * 36.0 + 16.0
}

pub fn view<'a>(app: &'a App, kind: Popover) -> El<'a> {
    match kind {
        Popover::Bell => bell(app),
        Popover::Pins => pins(app),
        Popover::Inbox => inbox(app),
    }
}

fn title<'a>(app: &App, label: &str) -> El<'a> {
    let p = app.theme.palette;
    container(
        text(label.to_owned())
            .size(16)
            .font(INTER_SEMIBOLD)
            .color(p.header_text),
    )
    .padding(Padding::from([12, 16]))
    .width(Length::Fill)
    .into()
}

fn section<'a>(app: &App, label: String) -> El<'a> {
    let p = app.theme.palette;
    container(
        text(label.to_uppercase())
            .size(12)
            .font(INTER_SEMIBOLD)
            .color(p.text_muted),
    )
    .padding(Padding::from([10, 16]).bottom(4))
    .into()
}

fn menu_row<'a>(app: &App, label: &str, checked: bool, msg: Ui) -> El<'a> {
    let p = app.theme.palette;
    let mut r = row![
        text(label.to_owned())
            .size(14)
            .font(INTER_MEDIUM)
            .width(Length::Fill)
    ]
    .align_y(Alignment::Center);
    if checked {
        r = r.push(icon(Icon::Check, 16.0, p.text_secondary));
    }
    button(r)
        .padding(Padding::from([8, 10]))
        .width(Length::Fill)
        .on_press(ui(msg))
        .style(theme::menu_item(p, false))
        .into()
}

fn channel_label(app: &App, c: &model::Channel) -> String {
    if c.kind.is_private() {
        format!("@{}", c.label(|u| app.chat.user_name(u)))
    } else {
        format!("#{}", c.name.as_deref().unwrap_or("channel"))
    }
}

fn bell(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let Some(c) = app.open_channel().and_then(|c| app.chat.channels.get(&c)) else {
        return container(text("Open a channel first").size(14).color(p.text_muted))
            .padding(16)
            .width(BELL_W)
            .style(theme::popup(p))
            .into();
    };
    let label = channel_label(app, c);
    let notify = app.chat.channel_notify(c.id);
    let muted = app.chat.is_channel_muted(c.id);
    let mut col = column![title(app, "Notification Settings")].padding(Padding::ZERO.bottom(8));

    col = col.push(section(app, format!("Mute {label}")));
    let mut list = column![].spacing(2).padding(Padding::from([0, 8]));
    if muted {
        let until = match notify.mute_end_ms {
            Some(end) => format!("Muted until {}", format_timestamp(end, now_ms())),
            None => "Muted until you turn it back on".to_owned(),
        };
        list = list.push(
            button(
                column![
                    text(format!("Unmute {label}"))
                        .size(14)
                        .font(INTER_MEDIUM)
                        .color(p.text_normal),
                    text(until).size(12).color(p.text_muted),
                ]
                .spacing(2),
            )
            .padding(Padding::from([8, 10]))
            .width(Length::Fill)
            .on_press(ui(Ui::UnmuteChannel(c.id)))
            .style(theme::menu_item(p, false)),
        );
    } else {
        for d in MuteDuration::ALL {
            list = list.push(menu_row(app, d.label(), false, Ui::MuteChannel(c.id, d)));
        }
    }
    col = col.push(list);

    col = col.push(section(app, "Notification Settings".into()));
    let mut levels = column![].spacing(2).padding(Padding::from([0, 8]));
    for l in [
        NotifyLevel::Default,
        NotifyLevel::All,
        NotifyLevel::Mentions,
        NotifyLevel::Nothing,
    ] {
        let label = if l == NotifyLevel::Default && c.kind.is_private() {
            "Use Default"
        } else {
            l.label()
        };
        levels = levels.push(menu_row(
            app,
            label,
            notify.level == l,
            Ui::SetChannelLevel(c.id, l),
        ));
    }
    col = col.push(levels);
    container(col).width(BELL_W).style(theme::popup(p)).into()
}

fn empty_state<'a>(app: &App, i: Icon, head: &str, sub: &str) -> El<'a> {
    let p = app.theme.palette;
    container(
        column![
            icon(i, 48.0, p.text_muted),
            text(head.to_owned())
                .size(16)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
            text(sub.to_owned())
                .size(14)
                .color(p.text_secondary)
                .align_x(Alignment::Center),
        ]
        .spacing(8)
        .align_x(Alignment::Center),
    )
    .padding(32)
    .center_x(Length::Fill)
    .into()
}

/// A message in a popover list: place line, author, time, text, actions.
fn message_card<'a>(app: &'a App, m: &'a Msg, place: String, actions: Vec<El<'a>>) -> El<'a> {
    let p = app.theme.palette;
    let now = now_ms();
    let snippet = truncate_chars(&super::plain_text(app, &m.content).replace('\n', " "), 220);
    let snippet = if snippet.is_empty() && !m.attachments.is_empty() {
        "Sent an attachment".to_owned()
    } else {
        snippet
    };
    let head = row![
        text(m.author.display_name().to_owned())
            .size(15)
            .font(INTER_MEDIUM)
            .color(p.header_text),
        text(format_timestamp(m.timestamp_ms(), now))
            .size(12)
            .color(p.text_muted),
        Space::new().width(Length::Fill),
    ]
    .extend(actions)
    .spacing(8)
    .align_y(Alignment::Center);
    column![
        text(place)
            .size(12)
            .font(INTER_SEMIBOLD)
            .color(p.text_muted),
        container(
            row![
                avatar(app, &m.author, 32.0),
                column![head, text(snippet).size(14).color(p.text_normal)]
                    .spacing(2)
                    .width(Length::Fill),
            ]
            .spacing(12)
        )
        .padding(10)
        .width(Length::Fill)
        .style(theme::rounded_bg(p.bg_panel, 6.0)),
    ]
    .spacing(4)
    .into()
}

fn small_button<'a>(app: &App, label: &'static str, msg: Ui) -> El<'a> {
    let p = app.theme.palette;
    button(
        text(label)
            .size(12)
            .font(INTER_SEMIBOLD)
            .color(p.text_normal),
    )
    .padding(Padding::from([3, 8]))
    .on_press(ui(msg))
    .style(theme::icon_button(p))
    .into()
}

fn scroller<'a>(app: &App, col: iced::widget::Column<'a, crate::app::Message>) -> El<'a> {
    scrollable(col)
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new().width(4).scroller_width(4),
        ))
        .style(theme::thin_scrollbar(app.theme.palette, false))
        .height(Length::Shrink)
        .into()
}

fn pins(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let s = &app.panels.pins;
    let channel = s.channel;
    let can_unpin = channel.is_some_and(|c| app.chat.can_manage_messages(c));
    let mut col = column![].spacing(8).padding(Padding::from([4, 12]));
    if s.loading && s.items.is_empty() {
        col = col.push(
            container(text("Loading\u{2026}").size(14).color(p.text_muted))
                .padding(24)
                .center_x(Length::Fill),
        );
    } else if let Some(e) = &s.error {
        col = col.push(
            container(text(e.clone()).size(14).color(p.danger))
                .padding(24)
                .center_x(Length::Fill),
        );
    } else if s.items.is_empty() {
        col = col.push(empty_state(
            app,
            Icon::Pin,
            "No pinned messages",
            "This channel doesn't have any pinned messages\u{2026} yet.",
        ));
    }
    for m in &s.items {
        let place = app
            .chat
            .channels
            .get(&m.channel_id)
            .map(|c| channel_label(app, c))
            .unwrap_or_default();
        let mut actions: Vec<El<'_>> = vec![small_button(
            app,
            "Jump",
            Ui::JumpTo {
                channel: m.channel_id,
                message: m.id,
            },
        )];
        if can_unpin {
            actions.push(
                button(icon(Icon::PinOff, 16.0, p.text_secondary))
                    .padding(4)
                    .on_press(ui(Ui::Unpin(m.channel_id, m.id)))
                    .style(theme::icon_button(p))
                    .into(),
            );
        }
        col = col.push(message_card(app, m, place, actions));
    }
    let heading = if s.items.is_empty() {
        "Pinned Messages".to_owned()
    } else {
        format!("Pinned Messages ({})", s.items.len())
    };
    container(column![title(app, &heading), scroller(app, col)])
        .width(PINS_W)
        .max_height(MAX_H)
        .style(theme::popup(p))
        .into()
}

fn inbox(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let s = &app.panels.inbox;
    let in_guild = matches!(app.view, View::Guild { .. });
    let mut head = row![
        text("Inbox")
            .size(16)
            .font(INTER_SEMIBOLD)
            .color(p.header_text),
        text("Mentions").size(13).color(p.text_secondary),
        Space::new().width(Length::Fill),
    ]
    .spacing(12)
    .align_y(Alignment::Center);
    if in_guild {
        head = head.push(
            button(
                row![
                    icon(
                        if s.guild_only {
                            Icon::Check
                        } else {
                            Icon::Hash
                        },
                        14.0,
                        p.text_normal
                    ),
                    text("This Server").size(12).font(INTER_SEMIBOLD),
                ]
                .spacing(4)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([4, 8]))
            .on_press(ui(Ui::InboxGuildOnly(!s.guild_only)))
            .style(move |t, st| {
                let mut b = theme::icon_button(p)(t, st);
                if s.guild_only {
                    b.background = Some(iced::Background::Color(p.bg_selected));
                }
                b.text_color = p.text_normal;
                b
            }),
        );
    }
    let header = container(head)
        .padding(Padding::from([12, 16]))
        .width(Length::Fill);

    let mut col = column![].spacing(8).padding(Padding::from([4, 12]));
    if s.loading && s.items.is_empty() {
        col = col.push(
            container(text("Loading\u{2026}").size(14).color(p.text_muted))
                .padding(24)
                .center_x(Length::Fill),
        );
    } else if let Some(e) = &s.error {
        col = col.push(
            container(text(e.clone()).size(14).color(p.danger))
                .padding(24)
                .center_x(Length::Fill),
        );
    } else if s.items.is_empty() {
        col = col.push(empty_state(
            app,
            Icon::Inbox,
            "Nothing to see here yet",
            "When someone mentions you, it will show up here.",
        ));
    }
    for m in &s.items {
        let place = match app.chat.channels.get(&m.channel_id) {
            Some(c) if c.kind.is_private() => channel_label(app, c),
            Some(c) => {
                let guild = c
                    .guild_id
                    .and_then(|g| app.chat.guild(g))
                    .map(|g| g.name.clone())
                    .unwrap_or_default();
                format!("{guild} \u{203A} {}", channel_label(app, c))
            }
            None => "Unknown channel".to_owned(),
        };
        col = col.push(message_card(
            app,
            m,
            place,
            vec![small_button(
                app,
                "Jump",
                Ui::JumpTo {
                    channel: m.channel_id,
                    message: m.id,
                },
            )],
        ));
    }
    if s.has_more && !s.loading {
        col = col.push(
            button(text("Load more").size(13).color(p.accent_text))
                .padding(Padding::from([6, 14]))
                .on_press(ui(Ui::InboxMore))
                .style(theme::primary_button(p)),
        );
    } else if s.loading && !s.items.is_empty() {
        col = col.push(text("Loading\u{2026}").size(13).color(p.text_muted));
    }
    container(column![header, scroller(app, col)])
        .width(INBOX_W)
        .max_height(MAX_H)
        .style(theme::popup(p))
        .into()
}
