//! Popups drawn over the main view: message context menu, emoji picker,
//! delete confirmation and the status menu.

use iced::widget::{
    Space, button, column, container, image, mouse_area, opaque, row, scrollable, text, text_input,
};
use iced::{Alignment, Color, Length, Padding, Point, Size};

use super::icons::{Icon, icon};
use super::theme::{self, INTER, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar, ui};
use crate::app::actions::OpenPicker;
use crate::app::emoji::{self, CATEGORIES, PickerTarget, Section};
use crate::app::{App, Message, Ui, emoji_url, input};
use crate::net::model::*;

const MENU_W: f32 = 220.0;
const PICKER_W: f32 = 352.0;
const PICKER_H: f32 = 400.0;
const COLUMNS: usize = 9;

/// Number of popup slots [`layers`] always returns.
pub const SLOTS: usize = 6;

/// The popup layers, bottom to top, always exactly [`SLOTS`] long: a closed
/// popup is an empty placeholder so the widget tree keeps its shape (see
/// `ui::view`).
pub fn layers(app: &App) -> Vec<El<'_>> {
    let window = input::window_size();
    let status = if app.status_menu {
        let size = Size::new(240.0, 188.0);
        let at = Point::new(
            8.0,
            window.height - app.theme.metrics.user_panel_height - size.height - 8.0,
        );
        popup_layer(app, status_menu(app), at, Ui::ToggleStatusMenu)
    } else {
        super::empty()
    };
    let menu = match &app.menu {
        Some(mn) => {
            let h = menu_height(app, mn.message);
            let at = input::clamp_popup(mn.at, Size::new(MENU_W, h), window);
            popup_layer(app, context_menu(app, mn.message), at, Ui::CloseMenu)
        }
        None => super::empty(),
    };
    let picker = match &app.emoji_picker {
        Some(open) => {
            let at = picker_position(open, window);
            popup_layer(app, emoji_picker(app, open), at, Ui::CloseEmojiPicker)
        }
        None => super::empty(),
    };
    let delete = match app.pending_delete {
        Some((c, m)) => delete_dialog(app, c, m),
        None => super::empty(),
    };
    let popover = match app.panels.open {
        Some(kind) => {
            let size = super::popovers::size_of(app, kind);
            let at = super::popovers::position(app, size);
            // The backdrop starts under the header so its buttons stay
            // clickable (switching from one popover to another).
            popup_layer_from(
                app,
                super::popovers::view(app, kind),
                at,
                Ui::ClosePopover,
                app.theme.metrics.header_height,
            )
        }
        None => super::empty(),
    };
    let rail_menu = match super::rail::menu(app) {
        Some((card, at)) => popup_layer(app, card, at, Ui::CloseRailMenu),
        None => super::empty(),
    };
    vec![status, menu, picker, delete, popover, rail_menu]
}

fn picker_position(open: &OpenPicker, window: Size) -> Point {
    let size = Size::new(PICKER_W, PICKER_H);
    let at = match open.picker.target {
        // Opened from the composer: above and to the left of the button.
        PickerTarget::Composer => Point::new(
            open.at.x - size.width + 16.0,
            open.at.y - size.height - 12.0,
        ),
        // Opened from a message toolbar: below-left of the button.
        PickerTarget::Reaction { .. } => {
            Point::new(open.at.x - size.width + 16.0, open.at.y + 16.0)
        }
    };
    input::clamp_popup(at, size, window)
}

/// A click-away backdrop plus the popup at `at`.
fn popup_layer<'a>(app: &'a App, card: El<'a>, at: Point, close: Ui) -> El<'a> {
    popup_layer_from(app, card, at, close, 0.0)
}

/// Like [`popup_layer`], but the backdrop leaves the top `top` pixels
/// (the channel header) alone.
fn popup_layer_from<'a>(app: &'a App, card: El<'a>, at: Point, close: Ui, top: f32) -> El<'a> {
    let _ = app;
    // `Idle` makes the layers below see a "levitating" cursor while the
    // popup is open: no row under the backdrop reacts to hover. Without it
    // the message row under the pointer toggled its hover toolbar every
    // time the pointer crossed the menu's edge (the menu opens exactly at
    // the pointer), which showed as flicker.
    let backdrop = container(
        mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
            .on_press(ui(close.clone()))
            .on_right_press(ui(close))
            .interaction(iced::mouse::Interaction::Idle),
    )
    .padding(Padding::ZERO.top(top))
    .width(Length::Fill)
    .height(Length::Fill);
    let positioned = container(opaque(card))
        .padding(Padding::ZERO.top(at.y).left(at.x))
        .width(Length::Fill)
        .height(Length::Fill);
    iced::widget::stack![backdrop, positioned].into()
}

fn menu_height(app: &App, m: MessageId) -> f32 {
    let n = menu_items(app, m).len() as f32;
    n * 32.0 + 16.0
}

struct Item {
    icon: Icon,
    label: &'static str,
    action: Ui,
    danger: bool,
}

fn menu_items(app: &App, m: MessageId) -> Vec<Item> {
    let Some(c) = app.open_channel() else {
        return vec![];
    };
    let Some(msg) = app.chat.message(c, m) else {
        return vec![];
    };
    let mine = Some(msg.author.id) == app.chat.me_id();
    let mut v = vec![Item {
        icon: Icon::SmilePlus,
        label: "Add Reaction",
        action: Ui::OpenEmojiPicker(PickerTarget::Reaction {
            channel: c,
            message: m,
        }),
        danger: false,
    }];
    if mine && matches!(msg.kind, 0 | 19) {
        v.push(Item {
            icon: Icon::Pencil,
            label: "Edit Message",
            action: Ui::StartEdit(m),
            danger: false,
        });
    }
    v.push(Item {
        icon: Icon::Reply,
        label: "Reply",
        action: Ui::StartReply(m),
        danger: false,
    });
    if !msg.content.is_empty() {
        v.push(Item {
            icon: Icon::Copy,
            label: "Copy Text",
            action: Ui::CopyText(m),
            danger: false,
        });
    }
    v.push(Item {
        icon: Icon::Link,
        label: "Copy Message Link",
        action: Ui::CopyLink(m),
        danger: false,
    });
    if mine {
        v.push(Item {
            icon: Icon::Trash,
            label: "Delete Message",
            action: Ui::DeleteClicked(m),
            danger: true,
        });
    }
    v
}

fn context_menu(app: &App, m: MessageId) -> El<'_> {
    let p = app.theme.palette;
    let mut col = column![].spacing(2).padding(8);
    for it in menu_items(app, m) {
        let fg = if it.danger {
            p.danger
        } else {
            p.text_secondary
        };
        col = col.push(
            button(
                row![
                    text(it.label)
                        .size(14)
                        .font(INTER_MEDIUM)
                        .width(Length::Fill),
                    icon(it.icon, 18.0, fg),
                ]
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([6, 8]))
            .width(Length::Fill)
            .on_press(ui(it.action))
            .style(theme::menu_item(p, it.danger)),
        );
    }
    container(col).width(MENU_W).style(theme::popup(p)).into()
}

fn status_menu(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let mut col = column![].spacing(2).padding(8);
    for s in [
        Presence::Online,
        Presence::Idle,
        Presence::Dnd,
        Presence::Offline,
    ] {
        let dot = container(Space::new())
            .width(10)
            .height(10)
            .style(theme::rounded_bg(theme::presence_color(&p, s), 5.0));
        let mut r = row![
            dot,
            text(s.label())
                .size(14)
                .font(INTER_MEDIUM)
                .width(Length::Fill)
        ]
        .spacing(10)
        .align_y(Alignment::Center);
        if app.chat.my_status == s {
            r = r.push(icon(Icon::Check, 16.0, p.text_secondary));
        }
        col = col.push(
            button(r)
                .padding(Padding::from([8, 10]))
                .width(Length::Fill)
                .on_press(ui(Ui::SetStatus(s)))
                .style(theme::menu_item(p, false)),
        );
    }
    container(col).width(240).style(theme::popup(p)).into()
}

fn emoji_picker<'a>(app: &'a App, open: &'a OpenPicker) -> El<'a> {
    let p = app.theme.palette;
    let pk = &open.picker;
    let guild = app
        .open_channel()
        .and_then(|c| app.chat.channels.get(&c))
        .and_then(|c| c.guild_id)
        .and_then(|g| app.chat.guild(g));

    let search = text_input("Find the perfect emoji", &pk.query)
        .id(iced::widget::Id::new("emoji-search"))
        .on_input(|s| Message::Ui(Ui::EmojiQuery(s)))
        .size(14)
        .font(INTER)
        .padding(Padding::from([8, 10]))
        .style(theme::input(p, p.bg_app));

    // Section tabs.
    let mut tabs = row![].spacing(2);
    let tab = |label: El<'a>, section: Section| -> El<'a> {
        let active = pk.section == section && pk.query.is_empty();
        button(container(label).center(28))
            .padding(0)
            .on_press(ui(Ui::EmojiSection(section)))
            .style(move |t, s| {
                let mut st = theme::icon_button(p)(t, s);
                if active {
                    st.background = Some(iced::Background::Color(p.bg_selected));
                }
                st
            })
            .into()
    };
    if let Some(g) = guild.filter(|g| g.emojis.iter().any(|e| e.available)) {
        tabs = tabs.push(tab(
            text(
                g.acronym()
                    .chars()
                    .next()
                    .map(String::from)
                    .unwrap_or_default(),
            )
            .size(14)
            .font(INTER_SEMIBOLD)
            .color(p.text_secondary)
            .into(),
            Section::Server(g.id),
        ));
    }
    const FACES: [&str; 9] = [
        "\u{1F600}",
        "\u{270B}",
        "\u{1F43B}",
        "\u{1F354}",
        "\u{26BD}",
        "\u{1F697}",
        "\u{1F4A1}",
        "\u{2764}",
        "\u{1F3C1}",
    ];
    for (i, face) in FACES.iter().enumerate() {
        tabs = tabs.push(tab(text(*face).size(16).into(), Section::Unicode(i)));
    }

    // Grid.
    let mut cells: Vec<El<'a>> = Vec::new();
    let custom: Vec<&CustomEmoji> = match (&pk.section, guild) {
        (_, Some(g)) if !pk.query.is_empty() => emoji::custom_matching(g, &pk.query),
        (Section::Server(_), Some(g)) => emoji::custom_matching(g, ""),
        _ => vec![],
    };
    for ce in custom {
        let face: El<'a> = match app.image(&emoji_url(&app.cfg.cdn_url, ce.id)) {
            Some(h) => image(h.clone()).width(28).height(28).into(),
            None => text(":").size(14).color(p.text_muted).into(),
        };
        cells.push(emoji_button(p, face, Emoji::custom(ce)));
    }
    let unicode: Vec<&'static emojis::Emoji> = if !pk.query.is_empty() {
        emoji::search(&pk.query)
    } else if let Section::Unicode(i) = pk.section {
        emoji::category(i)
    } else {
        vec![]
    };
    for e in unicode {
        cells.push(emoji_button(
            p,
            text(e.as_str()).size(24).into(),
            Emoji::unicode(e.as_str()),
        ));
    }
    let empty = cells.is_empty();
    let mut grid = column![].spacing(2);
    let mut line = row![].spacing(2);
    let mut n = 0;
    for c in cells {
        line = line.push(c);
        n += 1;
        if n == COLUMNS {
            grid = grid.push(line);
            line = row![].spacing(2);
            n = 0;
        }
    }
    if n > 0 {
        grid = grid.push(line);
    }
    if empty {
        grid = grid.push(
            container(text("No emoji found").size(14).color(p.text_muted))
                .padding(24)
                .center_x(Length::Fill),
        );
    }
    let title = if !pk.query.is_empty() {
        "Search results".to_owned()
    } else {
        match pk.section {
            Section::Server(_) => guild.map(|g| g.name.clone()).unwrap_or_default(),
            Section::Unicode(i) => CATEGORIES
                .get(i)
                .map(|c| c.1.to_owned())
                .unwrap_or_default(),
        }
    };
    container(
        column![
            container(search).padding(8),
            container(tabs).padding(Padding::from([0, 8])),
            container(
                text(title.to_uppercase())
                    .size(12)
                    .font(INTER_SEMIBOLD)
                    .color(p.text_muted)
            )
            .padding(Padding::from([8, 12]).bottom(4)),
            scrollable(container(grid).padding(Padding::from([0, 8])))
                .direction(scrollable::Direction::Vertical(
                    scrollable::Scrollbar::new().width(4).scroller_width(4),
                ))
                .style(theme::thin_scrollbar(p, false))
                .height(Length::Fill),
        ]
        .spacing(4),
    )
    .width(PICKER_W)
    .height(PICKER_H)
    .style(theme::popup(p))
    .into()
}

fn emoji_button<'a>(p: theme::Palette, face: El<'a>, emoji: Emoji) -> El<'a> {
    button(container(face).center(34))
        .padding(0)
        .on_press(ui(Ui::PickEmoji(emoji)))
        .style(theme::icon_button(p))
        .into()
}

/// "Delete Message" confirmation with a preview of the message.
fn delete_dialog(app: &App, c: ChannelId, m: MessageId) -> El<'_> {
    let p = app.theme.palette;
    let preview: El<'_> = match app.chat.message(c, m) {
        Some(msg) => container(
            row![
                avatar(app, &msg.author, 40.0),
                column![
                    text(msg.author.display_name().to_owned())
                        .size(15)
                        .font(INTER_MEDIUM)
                        .color(p.header_text),
                    text(truncate_chars(&msg.content.replace('\n', " "), 140))
                        .size(14)
                        .color(p.text_normal),
                ]
                .spacing(2)
                .width(Length::Fill),
            ]
            .spacing(12),
        )
        .padding(12)
        .width(Length::Fill)
        .style(move |_| container::Style {
            border: iced::Border {
                radius: 6.0.into(),
                width: 1.0,
                color: p.border,
            },
            ..Default::default()
        })
        .into(),
        None => Space::new().into(),
    };
    let card = container(
        column![
            text("Delete Message")
                .size(20)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
            text("Are you sure you want to delete this message?")
                .size(14)
                .color(p.text_secondary),
            preview,
            text("PROTIP: You can hold down shift when clicking delete message to bypass this confirmation entirely.")
                .size(12)
                .color(p.text_muted),
            row![
                Space::new().width(Length::Fill),
                button(text("Cancel").size(14).color(p.text_normal))
                    .padding(Padding::from([8, 16]))
                    .on_press(ui(Ui::CancelDelete))
                    .style(theme::icon_button(p)),
                button(text("Delete").size(14).color(p.accent_text))
                    .padding(Padding::from([8, 20]))
                    .on_press(ui(Ui::ConfirmDelete))
                    .style(move |_, status| {
                        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
                        button::Style {
                            background: Some(iced::Background::Color(if hovered {
                                Color { r: p.danger.r * 0.85, g: p.danger.g * 0.85, b: p.danger.b * 0.85, a: 1.0 }
                            } else {
                                p.danger
                            })),
                            text_color: p.accent_text,
                            border: iced::Border { radius: 4.0.into(), ..Default::default() },
                            ..Default::default()
                        }
                    }),
            ]
            .spacing(8),
        ]
        .spacing(12),
    )
    .width(440)
    .padding(16)
    .style(theme::rounded_bg(p.bg_panel, 8.0));
    opaque(
        mouse_area(
            container(opaque(card))
                .center(Length::Fill)
                .style(theme::bg(Color {
                    a: 0.7,
                    ..Color::BLACK
                })),
        )
        .on_press(ui(Ui::CancelDelete)),
    )
}
