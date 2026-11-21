//! Far-left server rail: Home (DMs) button, unread DMs, guild icons with
//! unread pills and mention badges, "add server".

use iced::widget::{
    Space, button, column, container, mouse_area, row, rule, scrollable, stack, text, tooltip,
};
use iced::{Alignment, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, initials_circle, mention_badge, ui};
use crate::app::folders;
use crate::app::rail::RailItem;
use crate::app::state::Badge;
use crate::app::{App, Ui, View};
use crate::net::model::{self, Guild, GuildFolder};

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
) -> El<'a> {
    let p = app.theme.palette;
    let size = app.theme.metrics.rail_icon;
    let b = button(container(content).center(size))
        .width(size)
        .height(size)
        .padding(0)
        .on_press(ui(on))
        .style(theme::rail_icon(p, selected, has_image));
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

fn tip<'a>(app: &App, content: El<'a>, name: &str) -> El<'a> {
    let p = app.theme.palette;
    let tip = container(
        text(name.to_owned())
            .size(16)
            .font(INTER_SEMIBOLD)
            .color(p.header_text),
    )
    .padding(Padding::from([8, 12]))
    .style(theme::rounded_bg(p.bg_elevated, 5.0));
    container(tooltip(content, tip, tooltip::Position::Right).gap(12))
        .width(app.theme.metrics.rail_width)
        .center_x(app.theme.metrics.rail_width)
        .into()
}

/// The icon of a server: its image or its acronym.
fn guild_face<'a>(app: &'a App, g: &Guild, size: f32, active: bool) -> (El<'a>, bool) {
    match app.guild_icon(g) {
        Some(h) => (
            iced::widget::image(h.clone())
                .width(size)
                .height(size)
                .border_radius(if active {
                    16.0 * size / 48.0
                } else {
                    size / 2.0
                })
                .into(),
            true,
        ),
        None => {
            let acr = g.acronym();
            let px = (if acr.chars().count() > 2 { 14.0 } else { 18.0 }) * size / 48.0;
            (text(acr).size(px).font(INTER_MEDIUM).into(), false)
        }
    }
}

/// Converts the rail button style into a container style, so the tile can
/// sit in a `mouse_area` (which carries the drag gestures) and still morph
/// on hover like the official rail.
fn tile_style(
    p: theme::Palette,
    selected: bool,
    has_image: bool,
    hot: bool,
    drop: bool,
    tint: Option<iced::Color>,
) -> impl Fn(&iced::Theme) -> container::Style {
    move |t| {
        let status = if hot {
            button::Status::Hovered
        } else {
            button::Status::Active
        };
        let s = theme::rail_icon(p, selected, has_image)(t, status);
        let mut background = s.background;
        if let Some(c) = tint {
            background = Some(iced::Background::Color(if hot || selected {
                c
            } else {
                iced::Color { a: 0.35, ..c }
            }));
        }
        container::Style {
            background,
            text_color: Some(s.text_color),
            border: iced::Border {
                width: if drop { 2.0 } else { 0.0 },
                color: p.accent,
                ..s.border
            },
            ..Default::default()
        }
    }
}

/// A rail entry that can be clicked, dragged onto another one and
/// right-clicked.
#[allow(clippy::too_many_arguments)]
fn draggable<'a>(
    app: &'a App,
    content: El<'a>,
    name: &str,
    item: RailItem,
    selected: bool,
    badge: Badge,
    has_image: bool,
    tint: Option<iced::Color>,
) -> El<'a> {
    let p = app.theme.palette;
    let size = app.theme.metrics.rail_icon;
    let hot = app.rail.hover == Some(item);
    let drop = app.rail.drop_target() == Some(item);
    let tile = container(content).center(size).style(tile_style(
        p,
        selected,
        has_image,
        hot || drop,
        drop,
        tint,
    ));
    let with_badge: El<'a> = if badge.mentions > 0 {
        stack![
            tile,
            container(mention_badge(&p, badge.mentions, p.bg_app))
                .width(size + 4.0)
                .height(size + 2.0)
                .align_right(size + 4.0)
                .align_bottom(size + 2.0),
        ]
        .into()
    } else {
        tile.into()
    };
    let area = mouse_area(with_badge)
        .on_press(ui(Ui::RailPress(item)))
        .on_release(ui(Ui::RailRelease(item)))
        .on_enter(ui(Ui::RailEnter(item)))
        .on_exit(ui(Ui::RailExit))
        .on_right_press(ui(Ui::RailMenu(item)))
        .interaction(iced::mouse::Interaction::Pointer);
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
        container(tooltip(area, tip, tooltip::Position::Right).gap(12))
            .width(Length::Fill)
            .center_x(Length::Fill)
    ]
    .width(app.theme.metrics.rail_width)
    .align_y(Alignment::Center)
    .into()
}

fn guild_item<'a>(app: &'a App, g: &'a Guild, _in_folder: bool) -> El<'a> {
    let selected = app.guild_open(g.id);
    let hot = app.rail.hover == Some(RailItem::Guild(g.id));
    let (content, has_image) = guild_face(app, g, 48.0, selected || hot);
    draggable(
        app,
        content,
        &g.name,
        RailItem::Guild(g.id),
        selected,
        app.chat.guild_badge(g.id),
        has_image,
        None,
    )
}

fn folder_color(_app: &App, f: &GuildFolder) -> iced::Color {
    theme::from_int(f.color.unwrap_or(folders::DEFAULT_COLOR))
}

/// A folder: collapsed it is one tile with up to four mini icons; open it
/// is the folder glyph followed by its servers on a tinted strip.
fn folder_view<'a>(app: &'a App, f: &'a GuildFolder, id: u64, members: &[&'a Guild]) -> El<'a> {
    let p = app.theme.palette;
    let color = folder_color(app, f);
    let open = app.rail.expanded.contains(&id);
    let mut badge = Badge::default();
    for g in members {
        let b = app.chat.guild_badge(g.id);
        badge.unread |= b.unread;
        badge.mentions += b.mentions;
    }
    let selected_member = members.iter().any(|g| app.guild_open(g.id));
    let label = folders::folder_label(f, |gid| app.chat.guild(gid).map(|g| g.name.as_str()));
    let content: El<'a> = if open {
        icon(Icon::Folder, 26.0, color)
    } else {
        let mini = |g: Option<&&'a Guild>| -> El<'a> {
            match g {
                Some(g) => match app.guild_icon(g) {
                    Some(h) => iced::widget::image(h.clone())
                        .width(18)
                        .height(18)
                        .border_radius(9.0)
                        .into(),
                    None => initials_circle(&p, &g.name, theme::default_avatar_color(g.id.0), 18.0),
                },
                None => Space::new().width(18).height(18).into(),
            }
        };
        column![
            row![mini(members.first()), mini(members.get(1))].spacing(2),
            row![mini(members.get(2)), mini(members.get(3))].spacing(2),
        ]
        .spacing(2)
        .into()
    };
    let header = draggable(
        app,
        content,
        &label,
        RailItem::Folder(id),
        selected_member && !open,
        if open { Badge::default() } else { badge },
        true,
        Some(color),
    );
    if !open {
        return header;
    }
    let mut strip = column![header].spacing(8).align_x(Alignment::Center);
    for g in members {
        strip = strip.push(guild_item(app, g, true));
    }
    container(strip)
        .padding(Padding::from([4, 0]))
        .style(theme::rounded_bg(iced::Color { a: 0.18, ..color }, 24.0))
        .width(app.theme.metrics.rail_width - 8.0)
        .into()
}

/// Right-click menu of a rail entry.
pub fn menu(app: &App) -> Option<(El<'_>, iced::Point)> {
    use super::context_menu::{Row, card, guild_rows};
    let m = app.rail.menu?;
    let mut rows: Vec<Row> = Vec::new();
    match m.item {
        RailItem::Guild(g) => {
            rows.extend(guild_rows(app, g));
            match app.chat.folders.folder_of(g) {
                None => rows.push(Row::item(
                    Icon::Folder,
                    "Create Folder",
                    Ui::CreateFolder(g),
                )),
                Some(_) => rows.push(Row::item(
                    Icon::X,
                    "Remove from Folder",
                    Ui::RemoveFromFolder(g),
                )),
            }
            for f in app.chat.folders.items.iter().filter(|f| f.id.is_some()) {
                let Some(id) = f.id else { continue };
                if f.guild_ids.contains(&g) {
                    continue;
                }
                let label =
                    folders::folder_label(f, |gid| app.chat.guild(gid).map(|g| g.name.as_str()));
                rows.push(Row::item(
                    Icon::Folder,
                    format!("Move to {}", model::truncate_chars(&label, 22)),
                    Ui::MoveToFolder(g, id),
                ));
            }
        }
        RailItem::Folder(f) => {
            rows.push(Row::item(
                Icon::Settings,
                "Folder Settings",
                Ui::OpenFolderSettings(f),
            ));
            rows.push(Row::danger(
                Icon::Trash,
                "Ungroup Servers",
                Ui::UngroupFolder(f),
            ));
        }
    }
    Some(card(app, rows, m.at))
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

    for entry in &app.chat.folders.items {
        let members: Vec<&Guild> = entry
            .guild_ids
            .iter()
            .filter_map(|id| app.chat.guild(*id))
            .collect();
        match entry.id {
            None => {
                if let Some(g) = members.first() {
                    col = col.push(guild_item(app, g, false));
                }
            }
            Some(id) if !members.is_empty() => {
                col = col.push(folder_view(app, entry, id, &members));
            }
            Some(_) => {}
        }
    }

    let add = button(container(icon(Icon::Plus, 24.0, p.success)).center(48))
        .width(48)
        .height(48)
        .padding(0)
        .on_press(ui(Ui::OpenAddServer))
        .style(theme::rail_add(p));
    let explore = button(container(icon(Icon::Compass, 24.0, p.success)).center(48))
        .width(48)
        .height(48)
        .padding(0)
        .on_press(ui(Ui::OpenDiscover))
        .style(theme::rail_add(p));
    col = col.push(tip(app, add.into(), "Add a Server"));
    col = col.push(tip(app, explore.into(), "Discover"));

    container(super::smooth_scroll::smooth(
        scrollable(col)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(0).scroller_width(0),
            ))
            .height(Length::Fill),
    ))
    .width(t.metrics.rail_width)
    .height(Length::Fill)
    .style(theme::bg(p.bg_app))
    .into()
}
