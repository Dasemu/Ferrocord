//! Search results panel (replaces the member list while searching).

use iced::widget::{Space, button, column, container, row, scrollable, text};
use iced::{Alignment, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar, format_timestamp, ui};
use crate::app::{App, Ui, now_ms};
use crate::net::model::{self as model, Message as Msg};

pub const WIDTH: f32 = 420.0;

pub fn view(app: &App) -> El<'_> {
    let p = app.theme.palette;
    let s = &app.search;
    let title = if s.loading {
        "Searching\u{2026}".to_owned()
    } else if let Some(e) = &s.error {
        e.clone()
    } else if s.total == 1 {
        "1 Result".to_owned()
    } else {
        format!("{} Results", s.total)
    };
    let header = container(
        row![
            text(title)
                .size(14)
                .font(INTER_SEMIBOLD)
                .color(p.header_text)
                .width(Length::Fill),
            button(icon(Icon::X, 18.0, p.text_secondary))
                .padding(4)
                .on_press(ui(Ui::CloseSearch))
                .style(theme::icon_button(p)),
        ]
        .align_y(Alignment::Center),
    )
    .height(app.theme.metrics.header_height)
    .padding(Padding::from([0, 16]))
    .align_y(Alignment::Center)
    .style(theme::header(p));

    let now = now_ms();
    let mut col = column![].spacing(8).padding(Padding::from([8, 8]));
    if !s.loading && s.error.is_none() && s.results.is_empty() {
        col = col.push(
            container(
                column![
                    icon(Icon::Search, 48.0, p.text_muted),
                    text("No results")
                        .size(16)
                        .font(INTER_SEMIBOLD)
                        .color(p.header_text),
                    text("We searched far and wide. Try different words.")
                        .size(14)
                        .color(p.text_secondary),
                ]
                .spacing(8)
                .align_x(Alignment::Center),
            )
            .padding(32)
            .center_x(Length::Fill),
        );
    }
    for m in &s.results {
        col = col.push(result_card(app, m, now));
    }
    let pager = row![
        pager_button(app, "Previous", s.has_prev(), Ui::SearchPage(false)),
        text(format!("Page {} of {}", s.page(), s.pages()))
            .size(12)
            .color(p.text_muted)
            .width(Length::Fill)
            .align_x(Alignment::Center),
        pager_button(app, "Next", s.has_next(), Ui::SearchPage(true)),
    ]
    .align_y(Alignment::Center)
    .padding(Padding::from([8, 12]));

    container(column![
        header,
        scrollable(col)
            .direction(scrollable::Direction::Vertical(
                scrollable::Scrollbar::new().width(4).scroller_width(4),
            ))
            .style(theme::thin_scrollbar(p, false))
            .height(Length::Fill),
        if s.total > crate::net::rest::SEARCH_PAGE {
            El::from(pager)
        } else {
            Space::new().into()
        },
    ])
    .width(WIDTH)
    .height(Length::Fill)
    .style(theme::bg(p.bg_sidebar))
    .into()
}

fn pager_button<'a>(app: &App, label: &'static str, enabled: bool, msg: Ui) -> El<'a> {
    let p = app.theme.palette;
    let b = button(text(label).size(13).color(p.accent_text))
        .padding(Padding::from([4, 12]))
        .style(theme::primary_button(p));
    if enabled { b.on_press(ui(msg)) } else { b }.into()
}

fn result_card<'a>(app: &'a App, m: &'a Msg, now: u64) -> El<'a> {
    let p = app.theme.palette;
    let channel = app.chat.channels.get(&m.channel_id);
    let place = match channel {
        Some(c) if c.kind.is_private() => c.label(|u| app.chat.user_name(u)),
        Some(c) => format!("#{}", c.name.as_deref().unwrap_or("channel")),
        None => "unknown channel".to_owned(),
    };
    let snippet =
        model::truncate_chars(&super::plain_text(app, &m.content).replace('\n', " "), 260);
    let snippet = if snippet.is_empty() && !m.attachments.is_empty() {
        "Sent an attachment".to_owned()
    } else {
        snippet
    };
    let body = column![
        row![
            text(m.author.display_name().to_owned())
                .size(15)
                .font(INTER_MEDIUM)
                .color(p.header_text),
            text(format_timestamp(m.timestamp_ms(), now))
                .size(12)
                .color(p.text_muted),
        ]
        .spacing(8)
        .align_y(Alignment::Center),
        text(snippet).size(14).color(p.text_normal),
    ]
    .spacing(2)
    .width(Length::Fill);
    column![
        text(place)
            .size(12)
            .font(INTER_SEMIBOLD)
            .color(p.text_muted),
        button(
            container(
                row![avatar(app, &m.author, 32.0), body]
                    .spacing(12)
                    .align_y(Alignment::Start)
            )
            .padding(10)
            .width(Length::Fill)
            .style(theme::rounded_bg(p.bg_panel, 6.0)),
        )
        .padding(0)
        .width(Length::Fill)
        .on_press(ui(Ui::JumpTo {
            channel: m.channel_id,
            message: m.id,
        }))
        .style(theme::link_button(p)),
    ]
    .spacing(4)
    .into()
}
