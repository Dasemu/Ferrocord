//! Far-left server rail: Home (DMs) button, unread DMs, guild icons with
//! unread pills and mention badges, "add server".

use iced::widget::{button, column, container, row, rule, scrollable, stack, text, tooltip};
use iced::{Alignment, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, initials_circle, mention_badge, ui};
use crate::app::state::Badge;
use crate::app::{App, Ui, View};

/// Indicator state for the pill at the very left of the rail.
#[derive(Clone, Copy)]
enum Pill {
    None,
    Unread,
    Selected,
}

fn pill<'a>(app: &App, kind: Pill) -> El<'a> {
    let p = app.theme.palette;
    let h = match kind {
        Pill::None => 0.0,
        Pill::Unread => 8.0,
        Pill::Selected => 40.0,
    };
    container(
        container(text(""))
            .width(4)
            .height(h)
            .style(move |_| container::Style {
                background: Some(iced::Background::Color(p.rail_pill)),
                border: iced::Border {
                    radius: iced::border::Radius::default().right(4.0),
                    ..Default::default()
                },
                ..Default::default()
            }),
    )
    .width(8)
    .height(48)
    .align_y(Alignment::Center)
    .into()
}

fn item<'a>(
    app: &'a App,
    content: El<'a>,
    name: &str,
    on: Ui,
    selected: bool,
    badge: Badge,
    has_image: bool,
    home: bool,
) -> El<'a> {
    let p = app.theme.palette;
    let size = app.theme.metrics.rail_icon;
    let b = button(container(content).center(size))
        .width(size)
        .height(size)
        .padding(0)
        .on_press(ui(on))
        .style(theme::rail_icon(p, selected, has_image, home));
    let with_badge: El<'a> = if badge.mentions > 0 {
        stack![
            b,
            container(mention_badge(&p, badge.mentions, p.bg_app))
                .width(size + 4.0)
                .height(size + 2.0)
                .align_right(size + 4.0)
                .align_bottom(size + 2.0),
        ]
        .into()
    } else {
        b.into()
    };
    let tip = container(
        text(name.to_owned())
            .size(16)
            .font(INTER_SEMIBOLD)
            .color(p.header_text),
    )
    .padding(Padding::from([8, 12]))
    .style(theme::rounded_bg(p.bg_elevated, 5.0));
    let kind = if selected {
        Pill::Selected
    } else if badge.unread {
        Pill::Unread
    } else {
        Pill::None
    };
    row![
        pill(app, kind),
        container(tooltip(with_badge, tip, tooltip::Position::Right).gap(12))
            .width(Length::Fill)
            .center_x(Length::Fill)
    ]
    .width(app.theme.metrics.rail_width)
    .align_y(Alignment::Center)
    .into()
}

pub fn view(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let home_selected = matches!(app.view, View::Home(_));
    let home_icon = icon(
        Icon::MessageCircle,
        28.0,
        if home_selected {
            p.accent_text
        } else {
            p.text_normal
        },
    );
    let mut col = column![item(
        app,
        home_icon,
        "Direct Messages",
        Ui::SelectHome,
        home_selected,
        Badge::default(),
        false,
        true
    )]
    .spacing(8)
    .padding(Padding::from([12, 0]))
    .align_x(Alignment::Center);

    // Unread DMs show up as their own entries under Home.
    for (c, n) in app.chat.unread_dms().into_iter().take(5) {
        let label = c.label(|u| app.chat.user_name(u));
        let content: El<'_> = match c.recipients.first().and_then(|u| app.chat.users.get(u)) {
            Some(u) if c.recipients.len() == 1 => super::avatar(app, u, 48.0),
            _ => initials_circle(&p, &label, theme::default_avatar_color(c.id.0), 48.0),
        };
        col = col.push(item(
            app,
            content,
            &label,
            Ui::SelectChannel(c.id),
            false,
            Badge {
                unread: true,
                mentions: n,
            },
            true,
            false,
        ));
    }

    col = col.push(
        container(rule::horizontal(2).style(move |_| rule::Style {
            color: p.bg_selected,
            radius: 1.0.into(),
            fill_mode: rule::FillMode::Full,
            snap: true,
        }))
        .width(32),
    );

    for g in &app.chat.guilds {
        let selected = matches!(app.view, View::Guild { guild, .. } if guild == g.id);
        let badge = app.chat.guild_badge(g.id);
        let (content, has_image): (El<'_>, bool) = match app.guild_icon(g) {
            Some(h) => (
                iced::widget::image(h.clone())
                    .width(48)
                    .height(48)
                    .border_radius(if selected { 16.0 } else { 24.0 })
                    .into(),
                true,
            ),
            None => {
                let acr = g.acronym();
                let size = if acr.chars().count() > 2 { 14.0 } else { 18.0 };
                (text(acr).size(size).font(INTER_MEDIUM).into(), false)
            }
        };
        col = col.push(item(
            app,
            content,
            &g.name,
            Ui::SelectGuild(g.id),
            selected,
            badge,
            has_image,
            false,
        ));
    }

    let add = button(container(icon(Icon::Plus, 24.0, p.success)).center(48))
        .width(48)
        .height(48)
        .padding(0)
        .style(theme::rail_add(p));
    let explore = button(container(icon(Icon::Compass, 24.0, p.success)).center(48))
        .width(48)
        .height(48)
        .padding(0)
        .style(theme::rail_add(p));
    col = col.push(add).push(explore);

    container(
        scrollable(col)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .height(Length::Fill),
    )
    .width(t.metrics.rail_width)
    .height(Length::Fill)
    .style(theme::bg(p.bg_app))
    .into()
}
