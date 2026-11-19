//! Second column: guild channel list (categories, unread/mention state) or
//! the DM list, plus the user panel at the bottom.

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar_with_presence, avatar_with_status, initials_circle, mention_badge, ui};
use crate::app::{App, Ui, View};
use crate::net::model::*;

pub fn view(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let (header, list): (El<'_>, El<'_>) = match app.view {
        View::Guild { guild, channel } => (
            guild_header(app, guild),
            guild_channels(app, guild, channel),
        ),
        View::Home(open) => (dm_header(app), dm_list(app, open)),
    };
    container(column![
        container(header)
            .height(t.metrics.header_height)
            .width(Length::Fill)
            .style(theme::sidebar_header(p)),
        scrollable(list)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .style(theme::thin_scrollbar(p, false))
            .height(Length::Fill),
        user_panel(app),
    ])
    .width(t.metrics.sidebar_width)
    .height(Length::Fill)
    .style(theme::bg(p.bg_sidebar))
    .into()
}

fn guild_header(app: &App, g: GuildId) -> El<'_> {
    let p = app.theme.palette;
    let name = app
        .chat
        .guild(g)
        .map(|g| g.name.clone())
        .unwrap_or_default();
    row![
        text(name)
            .size(16)
            .font(INTER_SEMIBOLD)
            .color(p.header_text)
            .width(Length::Fill),
        icon(Icon::ChevronDown, 18.0, p.text_normal),
    ]
    .padding(Padding::from([0, 16]))
    .height(Length::Fill)
    .align_y(Alignment::Center)
    .into()
}

fn channel_icon(kind: ChannelKind) -> Icon {
    match kind {
        ChannelKind::Voice | ChannelKind::Stage => Icon::Volume2,
        ChannelKind::Announcement => Icon::Megaphone,
        ChannelKind::Forum => Icon::MessageSquareText,
        _ => Icon::Hash,
    }
}

/// Unread pill on the left edge of the sidebar.
fn edge_pill<'a>(app: &App, show: bool) -> El<'a> {
    let p = app.theme.palette;
    container(if show {
        container(text(""))
            .width(4)
            .height(8)
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(p.rail_pill)),
                border: iced::Border {
                    radius: iced::border::Radius::default().right(4.0),
                    ..Default::default()
                },
                ..Default::default()
            })
            .into()
    } else {
        El::from(Space::new().width(4))
    })
    .width(8)
    .height(32)
    .align_y(Alignment::Center)
    .into()
}

fn channel_row<'a>(app: &'a App, c: &'a Channel, selected: bool) -> El<'a> {
    let p = app.theme.palette;
    let unread =
        app.chat.is_unread(c.id) && !app.chat.muted_channels.contains(&c.id) && c.kind.is_textual();
    let mentions = app.chat.mention_count(c.id);
    let bright = selected || unread || mentions > 0;
    let color = if bright {
        p.header_text
    } else {
        p.channel_icon
    };
    let font = if unread && !selected {
        INTER_SEMIBOLD
    } else {
        INTER_MEDIUM
    };
    let mut r = row![
        icon(
            channel_icon(c.kind),
            20.0,
            if selected {
                p.text_secondary
            } else {
                p.channel_icon
            }
        ),
        text(c.name.clone().unwrap_or_default())
            .size(16)
            .font(font)
            .color(color)
            .width(Length::Fill)
            .wrapping(text::Wrapping::None),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if mentions > 0 {
        r = r.push(mention_badge(&p, mentions, p.bg_sidebar));
    }
    row![
        edge_pill(app, unread && !selected),
        button(r)
            .padding(Padding::from([6, 8]))
            .width(Length::Fill)
            .on_press(ui(Ui::SelectChannel(c.id)))
            .style(theme::list_row(p, selected)),
    ]
    .padding(Padding::ZERO.right(8))
    .align_y(Alignment::Center)
    .into()
}

fn guild_channels(app: &App, g: GuildId, open: Option<ChannelId>) -> El<'_> {
    let p = app.theme.palette;
    let mut col = column![]
        .spacing(2)
        .padding(Padding::ZERO.top(12).bottom(8));
    for (cat, kids) in app.chat.channel_tree(g) {
        if let Some(cat) = cat {
            let collapsed = app.collapsed.contains(&cat.id);
            let header = button(
                row![
                    icon(
                        if collapsed {
                            Icon::ChevronRight
                        } else {
                            Icon::ChevronDown
                        },
                        12.0,
                        p.text_muted
                    ),
                    text(cat.name.clone().unwrap_or_default().to_uppercase())
                        .size(12)
                        .font(INTER_SEMIBOLD)
                        .color(p.text_muted),
                ]
                .spacing(2)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([0, 0]).top(16).bottom(4).left(10))
            .width(Length::Fill)
            .on_press(ui(Ui::ToggleCategory(cat.id)))
            .style(theme::link_button(p));
            col = col.push(header);
            for c in kids {
                let selected = open == Some(c.id);
                // Collapsed categories still show unread and selected channels.
                if collapsed && !selected && !app.chat.is_unread(c.id) {
                    continue;
                }
                col = col.push(channel_row(app, c, selected));
            }
        } else {
            for c in kids {
                col = col.push(channel_row(app, c, open == Some(c.id)));
            }
        }
    }
    col.into()
}

fn dm_header(app: &App) -> El<'_> {
    let p = app.theme.palette;
    container(
        container(
            text("Find or start a conversation")
                .size(14)
                .font(INTER_MEDIUM)
                .color(p.text_muted),
        )
        .width(Length::Fill)
        .height(28)
        .padding(Padding::from([0, 6]))
        .align_y(Alignment::Center)
        .style(theme::rounded_bg(p.bg_app, 4.0)),
    )
    .padding(Padding::from([0, 10]))
    .height(Length::Fill)
    .align_y(Alignment::Center)
    .into()
}

fn dm_list(app: &App, open: Option<ChannelId>) -> El<'_> {
    let p = app.theme.palette;
    let friends = button(
        row![
            icon(Icon::Users, 22.0, p.text_secondary),
            text("Friends")
                .size(16)
                .font(INTER_MEDIUM)
                .color(p.text_secondary),
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([10, 12]))
    .width(Length::Fill)
    .style(theme::list_row(p, open.is_none()));
    let mut col = column![
        container(friends).padding(Padding::from([0, 8])),
        row![
            text("DIRECT MESSAGES")
                .size(12)
                .font(INTER_SEMIBOLD)
                .color(p.text_muted)
                .width(Length::Fill),
            icon(Icon::Plus, 16.0, p.text_muted),
        ]
        .padding(Padding::from([0, 18]).top(18).bottom(4).left(18))
        .align_y(Alignment::Center),
    ]
    .spacing(2)
    .padding(Padding::ZERO.top(8));
    for c in app.chat.dm_list() {
        col = col.push(dm_row(app, c, open == Some(c.id)));
    }
    col.into()
}

fn dm_row<'a>(app: &'a App, c: &'a Channel, selected: bool) -> El<'a> {
    let p = app.theme.palette;
    let label = c.label(|u| app.chat.user_name(u));
    let unread = app.chat.mention_count(c.id) > 0;
    let mentions = app.chat.mention_count(c.id);
    let pic: El<'a> = match (
        c.kind,
        c.recipients.first().and_then(|u| app.chat.users.get(u)),
    ) {
        (ChannelKind::Dm, Some(u)) => {
            avatar_with_presence(app, u, 32.0, app.chat.presence_of(u.id), p.bg_sidebar)
        }
        _ => initials_circle(&p, &label, theme::default_avatar_color(c.id.0), 32.0),
    };
    let mut names = column![
        text(label)
            .size(16)
            .font(if unread { INTER_SEMIBOLD } else { INTER_MEDIUM })
            .color(if selected || unread {
                p.header_text
            } else {
                p.channel_icon
            })
            .wrapping(text::Wrapping::None)
    ];
    if c.kind == ChannelKind::GroupDm {
        names = names.push(
            text(format!("{} Members", c.recipients.len() + 1))
                .size(12)
                .font(INTER)
                .color(p.text_muted),
        );
    }
    let mut r = row![pic, names.width(Length::Fill)]
        .spacing(12)
        .align_y(Alignment::Center);
    if mentions > 0 {
        r = r.push(mention_badge(&p, mentions, p.bg_sidebar));
    }
    container(
        button(r)
            .padding(Padding::from([6, 8]))
            .width(Length::Fill)
            .on_press(ui(Ui::SelectChannel(c.id)))
            .style(theme::list_row(p, selected)),
    )
    .padding(Padding::from([0, 8]))
    .into()
}

/// Width of every user-panel icon button.
const PANEL_BTN: f32 = 28.0;
const PANEL_GAP: f32 = 2.0;
const PANEL_PAD: f32 = 8.0;
/// Buttons in the panel: mic, deafen, settings.
const PANEL_BUTTONS: f32 = 3.0;

/// Width available to the name / status text next to the avatar.
pub fn panel_text_width(sidebar_width: f32) -> f32 {
    const AVATAR: f32 = 32.0;
    const IDENTITY_PAD: f32 = 4.0;
    const IDENTITY_GAP: f32 = 8.0;
    let buttons = PANEL_BUTTONS * PANEL_BTN + PANEL_BUTTONS * PANEL_GAP;
    (sidebar_width - 2.0 * PANEL_PAD - buttons - 2.0 * IDENTITY_PAD - AVATAR - IDENTITY_GAP)
        .max(0.0)
}

/// Bottom-left user panel like the official client: identity on the left
/// (name and status truncated with an ellipsis inside a fixed-width area,
/// so they can never run under the buttons), then fixed-size icon buttons.
/// There is no voice, so mic and deafen are shown disabled with a tooltip.
fn user_panel(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let me = app.chat.me.clone().unwrap_or(User {
        id: Snowflake(0),
        username: String::new(),
        global_name: None,
        avatar: None,
        bot: false,
    });
    let small = |i: Icon, on: Option<Ui>| {
        let enabled = on.is_some();
        let b = button(
            container(icon(
                i,
                18.0,
                if enabled {
                    p.text_secondary
                } else {
                    p.text_muted
                },
            ))
            .center(PANEL_BTN),
        )
        .width(PANEL_BTN)
        .height(PANEL_BTN)
        .padding(0)
        .style(theme::icon_button(p));
        match on {
            Some(u) => b.on_press(ui(u)),
            None => b,
        }
    };
    let disabled = |i: Icon, tip: &'static str| -> El<'_> {
        iced::widget::tooltip(
            small(i, None),
            container(text(tip).size(13).color(p.header_text))
                .padding(Padding::from([6, 10]))
                .style(theme::rounded_bg(p.bg_elevated, 5.0)),
            iced::widget::tooltip::Position::Top,
        )
        .gap(6)
        .into()
    };
    let avail = panel_text_width(t.metrics.sidebar_width);
    // Clicking the avatar or name opens the status menu, like the
    // official user panel.
    let identity = button(
        row![
            avatar_with_status(app, &me, 32.0),
            container(column![
                text(super::ellipsize(me.display_name(), avail, 14.0, true))
                    .size(14)
                    .font(INTER_SEMIBOLD)
                    .color(p.header_text)
                    .wrapping(text::Wrapping::None),
                text(super::ellipsize(
                    app.chat.my_status.label(),
                    avail,
                    12.0,
                    false
                ))
                .size(12)
                .color(p.text_secondary)
                .wrapping(text::Wrapping::None),
            ])
            .width(Length::Fill)
            .clip(true),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([0, 4]))
    .width(Length::Fill)
    .on_press(ui(Ui::ToggleStatusMenu))
    .style(theme::icon_button(p));
    container(
        row![
            identity,
            disabled(Icon::Mic, "Voice is not supported in Ferrocord"),
            disabled(Icon::Headphones, "Voice is not supported in Ferrocord"),
            small(Icon::Settings, Some(Ui::OpenSettings)),
        ]
        .spacing(PANEL_GAP)
        .align_y(Alignment::Center),
    )
    .height(t.metrics.user_panel_height)
    .padding(Padding::from([0.0, PANEL_PAD]))
    .align_y(Alignment::Center)
    .style(theme::bg(p.user_panel))
    .into()
}
