//! Right-hand member list: role groups, presence dots.

use iced::widget::{Space, column, container, row, scrollable, text};
use iced::{Alignment, Color, Length, Padding};

use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar_with_presence, cosmetics};
use crate::app::anim::Hover;
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
    person_row(
        app,
        &m.user,
        m.display_name(),
        color,
        m.presence,
        guild.map(|g| g.id),
    )
}

/// Avatar with presence, name and (for bots) a BOT tag: one list row.
fn person_row<'a>(
    app: &'a App,
    user: &User,
    name: &str,
    color: Color,
    presence: Presence,
    guild: Option<GuildId>,
) -> El<'a> {
    let p = app.theme.palette;
    let mut name_row = row![cosmetics::name(app, user, name, 16.0, INTER_MEDIUM, color)]
        .spacing(6)
        .align_y(Alignment::Center);
    if let Some(tag) = cosmetics::tag_chip(app, user) {
        name_row = name_row.push(tag);
    }
    if user.bot {
        name_row = name_row.push(bot_tag(app));
    }
    let content: El<'a> = container(
        row![
            avatar_with_presence(app, user, 32.0, presence, p.bg_sidebar),
            name_row,
        ]
        .spacing(12)
        .align_y(Alignment::Center),
    )
    .padding(Padding::from([6, 8]))
    .width(Length::Fill)
    .into();
    let content = cosmetics::with_nameplate(app, user, content);
    let h = Hover {
        user: user.id,
        message: None,
    };
    iced::widget::mouse_area(cosmetics::clickable(content, user, guild))
        .on_enter(Message::Ui(Ui::HoverEnter(h)))
        .on_exit(Message::Ui(Ui::HoverLeave(h)))
        .into()
}

fn bot_tag<'a>(app: &App) -> El<'a> {
    let p = app.theme.palette;
    container(
        text("BOT")
            .size(10)
            .font(INTER_SEMIBOLD)
            .color(p.accent_text),
    )
    .padding(Padding::from([1, 4]))
    .style(theme::rounded_bg(p.accent, 3.0))
    .into()
}

/// Right-hand panel of an open DM: the other person's profile in a 1:1 DM,
/// the participant list in a group DM.
pub fn dm_view<'a>(app: &'a App, c: &Channel) -> El<'a> {
    let p = app.theme.palette;
    let people = app.chat.dm_participants(c);
    let body: El<'a> = match (c.kind, people.as_slice()) {
        (ChannelKind::Dm, [other]) => profile(app, other),
        _ => {
            let mut col = column![
                container(
                    text(format!("MEMBERS \u{2014} {}", people.len()))
                        .size(12)
                        .font(INTER_SEMIBOLD)
                        .color(p.text_muted)
                        .wrapping(text::Wrapping::None),
                )
                .padding(Padding::from([0, 8]).top(4).bottom(4))
            ]
            .padding(Padding::from([8, 8]).top(16));
            for u in people {
                let presence = app.chat.presence_of(u.id);
                let color = if presence == Presence::Offline {
                    Color {
                        a: 0.4,
                        ..p.text_secondary
                    }
                } else {
                    p.text_secondary
                };
                col = col.push(person_row(app, u, u.display_name(), color, presence, None));
            }
            scrollable(col)
                .direction(scrollable::Direction::Vertical(
                    scrollable::Scrollbar::new().width(4).scroller_width(4),
                ))
                .style(theme::thin_scrollbar(p, false))
                .height(Length::Fill)
                .into()
        }
    };
    container(body)
        .width(WIDTH)
        .height(Length::Fill)
        .style(theme::bg(p.bg_sidebar))
        .into()
}

/// Profile card of the other person in a 1:1 DM.
fn profile<'a>(app: &'a App, u: &User) -> El<'a> {
    super::profile::panel(app, u)
}
