//! Right-click menus: users, channels / DMs (and the shared card the
//! server menu in `ui::rail` is drawn with).

use iced::widget::{button, column, container, row, rule, text};
use iced::{Alignment, Length, Padding, Point, Size};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM};

use super::{El, ui};
use crate::app::context::CtxTarget;
use crate::app::{App, Ui, input};
use crate::net::extras::MuteDuration;
use crate::net::model::*;

pub const MENU_W: f32 = 220.0;
const ROW_H: f32 = 32.0;
const SEP_H: f32 = 9.0;

/// One menu line.
pub enum Row {
    Item {
        icon: Icon,
        label: String,
        action: Ui,
        danger: bool,
    },
    Separator,
}

impl Row {
    pub fn item(icon: Icon, label: impl Into<String>, action: Ui) -> Self {
        Self::Item {
            icon,
            label: label.into(),
            action,
            danger: false,
        }
    }

    pub fn danger(icon: Icon, label: impl Into<String>, action: Ui) -> Self {
        Self::Item {
            icon,
            label: label.into(),
            action,
            danger: true,
        }
    }
}

/// The menu card for `rows`, placed at `at` (clamped to the window).
pub fn card(app: &App, rows: Vec<Row>, at: Point) -> (El<'_>, Point) {
    let p = app.theme.palette;
    let mut height = 16.0;
    let mut col = column![].spacing(2).padding(8);
    for r in rows {
        match r {
            Row::Separator => {
                height += SEP_H;
                col = col.push(
                    container(rule::horizontal(1).style(move |_| rule::Style {
                        color: p.bg_selected,
                        radius: 0.0.into(),
                        fill_mode: rule::FillMode::Full,
                        snap: true,
                    }))
                    .padding(Padding::from([3, 4])),
                );
            }
            Row::Item {
                icon: i,
                label,
                action,
                danger,
            } => {
                height += ROW_H;
                let fg = if danger { p.danger } else { p.text_secondary };
                col = col.push(
                    button(
                        row![
                            text(label).size(14).font(INTER_MEDIUM).width(Length::Fill),
                            icon(i, 18.0, fg),
                        ]
                        .align_y(Alignment::Center),
                    )
                    .padding(Padding::from([6, 8]))
                    .width(Length::Fill)
                    .on_press(ui(action))
                    .style(theme::menu_item(p, danger)),
                );
            }
        }
    }
    let at = input::clamp_popup(at, Size::new(MENU_W, height), input::window_size());
    (
        container(col).width(MENU_W).style(theme::popup(p)).into(),
        at,
    )
}

/// The open user / channel menu.
pub fn view(app: &App) -> Option<(El<'_>, Point)> {
    let m = app.ctx_menu?;
    let rows = match m.target {
        CtxTarget::User { user, guild } => user_rows(app, user, guild),
        CtxTarget::Channel(c) => channel_rows(app, c),
    };
    if rows.is_empty() {
        return None;
    }
    Some(card(app, rows, m.at))
}

fn user_rows(app: &App, user: UserId, guild: Option<GuildId>) -> Vec<Row> {
    let mut rows = vec![Row::item(
        Icon::UserRound,
        "Profile",
        Ui::OpenProfile { user, guild },
    )];
    if app.open_channel().is_some() {
        rows.push(Row::item(Icon::AtSign, "Mention", Ui::MentionUser(user)));
    }
    rows.push(Row::Separator);
    if let Some(u) = app.chat.users.get(&user) {
        rows.push(Row::item(
            Icon::Copy,
            "Copy Username",
            Ui::CopyToClipboard(u.username.clone()),
        ));
    }
    // "Copy ID" items exist only in Developer Mode, like the official
    // client.
    if app.developer_mode() {
        rows.push(Row::item(
            Icon::Copy,
            "Copy User ID",
            Ui::CopyToClipboard(user.to_string()),
        ));
    }
    rows
}

fn channel_rows(app: &App, c: ChannelId) -> Vec<Row> {
    let Some(ch) = app.chat.channels.get(&c) else {
        return vec![];
    };
    let mut rows = Vec::new();
    if ch.kind == ChannelKind::Dm
        && let Some(u) = ch.recipients.first()
    {
        rows.push(Row::item(
            Icon::UserRound,
            "Profile",
            Ui::OpenProfile {
                user: *u,
                guild: None,
            },
        ));
    }
    if ch.kind.is_textual() {
        if app.chat.is_unread(c) || app.chat.mention_count(c) > 0 {
            rows.push(Row::item(
                Icon::Check,
                "Mark As Read",
                Ui::MarkChannelRead(c),
            ));
        }
        let what = if ch.guild_id.is_some() {
            "Channel"
        } else {
            "Conversation"
        };
        if app.chat.is_channel_muted(c) {
            rows.push(Row::item(
                Icon::Bell,
                format!("Unmute {what}"),
                Ui::UnmuteChannel(c),
            ));
        } else {
            rows.push(Row::item(
                Icon::BellOff,
                format!("Mute {what}"),
                Ui::MuteChannel(c, MuteDuration::Forever),
            ));
        }
    }
    if !app.developer_mode() {
        return rows;
    }
    if !rows.is_empty() {
        rows.push(Row::Separator);
    }
    if ch.kind == ChannelKind::Dm
        && let Some(u) = ch.recipients.first()
    {
        rows.push(Row::item(
            Icon::Copy,
            "Copy User ID",
            Ui::CopyToClipboard(u.to_string()),
        ));
    }
    rows.push(Row::item(
        Icon::Copy,
        "Copy Channel ID",
        Ui::CopyToClipboard(c.to_string()),
    ));
    rows
}

/// Server actions at the top of the rail menu.
pub fn guild_rows(app: &App, g: GuildId) -> Vec<Row> {
    let mut rows = Vec::new();
    let b = app.chat.guild_badge(g);
    if b.unread || b.mentions > 0 {
        rows.push(Row::item(Icon::Check, "Mark As Read", Ui::MarkGuildRead(g)));
    }
    if app.chat.muted_guilds.contains(&g) {
        rows.push(Row::item(
            Icon::Bell,
            "Unmute Server",
            Ui::MuteGuild(g, false),
        ));
    } else {
        rows.push(Row::item(
            Icon::BellOff,
            "Mute Server",
            Ui::MuteGuild(g, true),
        ));
    }
    let suppressed = app.chat.suppress_everyone.contains(&g);
    rows.push(Row::item(
        if suppressed {
            Icon::Check
        } else {
            Icon::AtSign
        },
        "Suppress @everyone",
        Ui::SuppressEveryone(g, !suppressed),
    ));
    if app.developer_mode() {
        rows.push(Row::Separator);
        rows.push(Row::item(
            Icon::Copy,
            "Copy Server ID",
            Ui::CopyToClipboard(g.to_string()),
        ));
    }
    rows.push(Row::Separator);
    rows
}
