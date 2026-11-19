//! Pins / inbox side panel (replaces the member list while open).

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Length, Padding};

use super::icons::{Icon, icon};
use super::search::result_card;
use super::theme::{self, INTER_SEMIBOLD};
use super::{El, ui};
use crate::app::panel::PanelKind;
use crate::app::{App, Ui, now_ms};

pub const WIDTH: f32 = 420.0;

pub fn view(app: &App, kind: PanelKind) -> El<'_> {
    let p = app.theme.palette;
    let panel = &app.panel;
    let (glyph, title, close, empty_title, empty_body, failed) = match kind {
        PanelKind::Pins(_) => (
            Icon::Pin,
            "Pinned Messages",
            Ui::TogglePins,
            "No pinned messages",
            "This channel doesn't have any pinned messages\u{2026} yet.",
            "Could not load pinned messages",
        ),
        PanelKind::Inbox => (
            Icon::Inbox,
            "Inbox \u{2014} Mentions",
            Ui::ToggleInbox,
            "No recent mentions",
            "When someone mentions you, it will show up here.",
            "Could not load your mentions",
        ),
    };
    let header = container(
        row![
            icon(glyph, 20.0, p.text_secondary),
            text(title)
                .size(14)
                .font(INTER_SEMIBOLD)
                .color(p.header_text)
                .width(Length::Fill),
            button(icon(Icon::X, 18.0, p.text_secondary))
                .padding(4)
                .on_press(ui(close))
                .style(theme::icon_button(p)),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
    )
    .height(app.theme.metrics.header_height)
    .padding(Padding::from([0, 16]))
    .align_y(Alignment::Center)
    .style(theme::header(p));

    let now = now_ms();
    let mut col = column![].spacing(8).padding(Padding::from([8, 8]));
    let notice = |title: &'static str, body: &'static str| {
        container(
            column![
                icon(glyph, 48.0, p.text_muted),
                text(title)
                    .size(16)
                    .font(INTER_SEMIBOLD)
                    .color(p.header_text),
                text(body).size(14).color(p.text_secondary),
            ]
            .spacing(8)
            .align_x(Alignment::Center),
        )
        .padding(32)
        .center_x(Length::Fill)
    };
    if panel.loading {
        col = col.push(notice("Loading\u{2026}", ""));
    } else if let Some(e) = &panel.error {
        col = col.push(notice(failed, ""));
        col = col.push(container(text(e.clone()).size(14).color(p.danger)).center_x(Length::Fill));
    } else if panel.messages.is_empty() {
        col = col.push(notice(empty_title, empty_body));
    }
    // Pins all live in the open channel; mentions come from anywhere, so
    // they are labelled with their channel.
    let show_place = kind == PanelKind::Inbox;
    for m in &panel.messages {
        col = col.push(result_card(app, m, now, show_place));
    }

    container(column![
        header,
        scrollable(col)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .style(theme::thin_scrollbar(p, false))
            .height(Length::Fill),
    ])
    .width(WIDTH)
    .height(Length::Fill)
    .style(theme::bg(p.bg_sidebar))
    .into()
}
