//! Pinned messages panel (replaces the member list while open).

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Length, Padding};

use super::icons::{Icon, icon};
use super::search::result_card;
use super::theme::{self, INTER_SEMIBOLD};
use super::{El, ui};
use crate::app::{App, Ui, now_ms};

pub const WIDTH: f32 = 420.0;

pub fn view(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let pins = &app.pins;
    let header = container(
        row![
            icon(Icon::Pin, 20.0, p.text_secondary),
            text("Pinned Messages")
                .size(14)
                .font(INTER_SEMIBOLD)
                .color(p.header_text)
                .width(Length::Fill),
            button(icon(Icon::X, 18.0, p.text_secondary))
                .padding(4)
                .on_press(ui(Ui::TogglePins))
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
    let notice = |title: String, body: &'static str| {
        container(
            column![
                icon(Icon::Pin, 48.0, p.text_muted),
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
    if pins.loading {
        col = col.push(notice("Loading\u{2026}".to_owned(), ""));
    } else if let Some(e) = &pins.error {
        col = col.push(notice("Could not load pinned messages".to_owned(), ""));
        col = col.push(container(text(e.clone()).size(14).color(p.danger)).center_x(Length::Fill));
    } else if pins.messages.is_empty() {
        col = col.push(notice(
            "No pinned messages".to_owned(),
            "This channel doesn't have any pinned messages\u{2026} yet.",
        ));
    }
    for m in &pins.messages {
        col = col.push(result_card(app, m, now, false));
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
