//! Right-hand member list: role groups, presence dots.

use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Alignment, Color, Length, Padding};

use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar_with_presence};
use crate::app::members::MemberListState;
use crate::app::{App, Message, Ui};
use crate::net::model::*;

pub const WIDTH: f32 = 240.0;

pub fn view(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let Some(list) = &app.chat.member_list else {
        return Space::new().width(0).into();
    };
    let guild = app.chat.guild(list.guild);
    let mut col = column![].padding(Padding::from([8, 8]).top(16));
    let mut loaded = false;
    for item in list.loaded() {
        loaded = true;
        match item {
            MemberItem::Group { id, count } => {
                if id.is_empty() {
                    continue;
                }
                col = col.push(
                    container(
                        text(format!(
                            "{} \u{2014} {count}",
                            MemberListState::group_label(id, guild).to_uppercase()
                        ))
                        .size(12)
                        .font(INTER_SEMIBOLD)
                        .color(p.text_muted)
                        .wrapping(text::Wrapping::None),
                    )
                    .padding(Padding::from([0, 8]).top(20).bottom(4)),
                );
            }
            MemberItem::Member(m) => col = col.push(member_row(app, m, guild)),
        }
    }
    if !loaded {
        col = col.push(
            container(text("Loading members\u{2026}").size(14).color(p.text_muted))
                .padding(16)
                .center_x(Length::Fill),
        );
    }
    container(
        scrollable(col)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .on_scroll(|vp| Message::Ui(Ui::MembersScrolled(vp)))
            .style(theme::thin_scrollbar(p, false))
            .height(Length::Fill),
    )
    .width(WIDTH)
    .height(Length::Fill)
    .style(theme::bg(p.bg_sidebar))
    .into()
}

/// Colour of the highest role with a colour.
fn role_color(guild: Option<&Guild>, roles: &[RoleId]) -> Option<u32> {
    guild?
        .roles
        .iter()
        .filter(|r| r.color != 0 && roles.contains(&r.id))
        .max_by_key(|r| r.position)
        .map(|r| r.color)
}

fn member_row<'a>(app: &'a App, m: &'a MemberEntry, guild: Option<&Guild>) -> El<'a> {
    let p = app.theme.palette;
    let offline = m.presence == Presence::Offline;
    let base = role_color(guild, &m.roles)
        .map(theme::from_int)
        .unwrap_or(p.text_secondary);
    let color = if offline {
        Color { a: 0.4, ..base }
    } else {
        base
    };
    let mut name = row![
        text(m.display_name().to_owned())
            .size(16)
            .font(INTER_MEDIUM)
            .color(color)
            .wrapping(text::Wrapping::None)
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if m.user.bot {
        name = name.push(
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
    container(
        row![
            avatar_with_presence(app, &m.user, 32.0, m.presence, p.bg_sidebar),
            name,
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 8]))
    .width(Length::Fill)
    .into()
}
