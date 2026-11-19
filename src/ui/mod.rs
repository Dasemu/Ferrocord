//! Views: pure functions `&App -> Element<Message>`.

pub mod chat;
pub mod icons;
pub mod login;
pub mod markdown;
pub mod members;
pub mod pins;
pub mod popups;
pub mod rail;
pub mod search;
pub mod settings;
pub mod sidebar;
pub mod theme;

use chrono::{DateTime, Datelike, Local, TimeZone};
use iced::widget::{column, container, row, stack, text};
use iced::{Alignment, Element, Length, Padding};

use crate::app::{App, Message, Screen, Ui};
use crate::net::ConnectionState;
use crate::net::model::User;
use theme::{FerroTheme, INTER_SEMIBOLD, Palette};

pub type El<'a> = Element<'a, Message>;

/// View of the (only) main window, in the shape `iced::daemon` expects.
pub fn view_window(app: &App, _window: iced::window::Id) -> El<'_> {
    view(app)
}

pub fn view(app: &App) -> El<'_> {
    let t = &app.theme;
    let body: El<'_> = match app.screen() {
        Screen::Splash => splash(t),
        Screen::Login => login::view(app),
        Screen::Main => {
            let mut r = row![rail::view(app), sidebar::view(app), chat::view(app)];
            if app.search.panel_visible() {
                r = r.push(search::view(app));
            } else if app.pins.is_open_for(app.open_channel()) {
                r = r.push(pins::view(app));
            } else if app.members_open {
                match app.view {
                    crate::app::View::Guild {
                        channel: Some(_), ..
                    } => r = r.push(members::view(app)),
                    crate::app::View::Home(Some(c)) => {
                        if let Some(c) = app.chat.channels.get(&c) {
                            r = r.push(members::dm_view(app, c));
                        }
                    }
                    _ => {}
                }
            }
            r.height(Length::Fill).into()
        }
    };
    let banner = banner(app);
    let mut content: El<'_> = match banner {
        Some(b) => column![b, body].into(),
        None => body,
    };
    if app.screen() == Screen::Main {
        for layer in popups::layers(app) {
            content = stack![content, layer].into();
        }
    }
    if app.settings_open && app.screen() == Screen::Main {
        content = stack![content, settings::view(app)].into();
    }
    if let Some(url) = &app.pending_link {
        content = stack![content, leave_dialog(t, url)].into();
    }
    container(content)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(theme::bg(t.palette.bg_app))
        .into()
}

/// "Leaving Ferrocord" confirmation for masked links: shows the real URL.
fn leave_dialog<'a>(t: &FerroTheme, url: &str) -> El<'a> {
    let p = t.palette;
    let card = container(
        column![
            text("Leaving Ferrocord")
                .size(t.metrics.font_l)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
            text("This link is taking you to the following website")
                .size(t.metrics.font_s)
                .color(p.text_secondary),
            container(
                text(url.to_owned())
                    .size(t.metrics.font_s)
                    .color(p.text_normal)
            )
            .width(Length::Fill)
            .padding(10)
            .style(theme::rounded_bg(p.bg_app, 4.0)),
            row![
                iced::widget::Space::new().width(Length::Fill),
                iced::widget::button(text("Go Back").size(14).color(p.text_normal))
                    .padding(Padding::from([8, 16]))
                    .on_press(ui(Ui::CancelLink))
                    .style(theme::icon_button(p)),
                iced::widget::button(text("Visit Site").size(14).color(p.accent_text))
                    .padding(Padding::from([8, 16]))
                    .on_press(ui(Ui::OpenUrl(url.to_owned())))
                    .style(theme::primary_button(p)),
            ]
            .spacing(8),
        ]
        .spacing(12),
    )
    .width(440)
    .padding(16)
    .style(theme::rounded_bg(p.bg_panel, 8.0));
    iced::widget::opaque(
        iced::widget::mouse_area(
            container(iced::widget::opaque(card))
                .center(Length::Fill)
                .style(theme::bg(iced::Color {
                    a: 0.7,
                    ..iced::Color::BLACK
                })),
        )
        .on_press(ui(Ui::CancelLink)),
    )
}

fn splash(t: &FerroTheme) -> El<'static> {
    let p = t.palette;
    container(
        column![
            icons::icon(icons::Icon::MessageCircle, 56.0, p.accent),
            text("Ferrocord")
                .size(t.metrics.font_xl)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
            text("Connecting…")
                .size(t.metrics.font_s)
                .color(p.text_muted),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .center(Length::Fill)
    .into()
}

/// Thin banner while not connected (or for an error toast).
fn banner(app: &App) -> Option<El<'_>> {
    let p = app.theme.palette;
    if app.screen() != Screen::Main {
        return None;
    }
    let (msg, color) = match (&app.toast, app.connection) {
        (_, ConnectionState::Connecting) => ("Connecting to Discord…".to_string(), p.accent),
        (_, ConnectionState::Reconnecting { attempt, delay_ms }) => (
            format!(
                "Connection lost. Reconnecting{}… (attempt {attempt})",
                if delay_ms > 0 {
                    format!(" in {}s", delay_ms.div_ceil(1000))
                } else {
                    String::new()
                }
            ),
            p.mention_bar,
        ),
        (Some(t), _) => (t.clone(), p.danger),
        (None, ConnectionState::Disconnected) if !app.cfg.mock => (
            "Offline — showing cached messages.".to_string(),
            p.badge_muted,
        ),
        _ => return None,
    };
    Some(
        container(
            row![
                icons::icon(icons::Icon::WifiOff, 14.0, p.accent_text),
                text(msg)
                    .size(app.theme.metrics.font_s)
                    .color(p.accent_text),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([6, 16]))
        .align_x(Alignment::Center)
        .style(theme::bg(color))
        .into(),
    )
}

/// Round avatar: the downloaded image, or a coloured circle with the
/// user's initial.
pub fn avatar<'a>(app: &'a App, user: &User, size: f32) -> El<'a> {
    if let Some(h) = app.avatar_handle(user) {
        return iced::widget::image(h.clone())
            .width(size)
            .height(size)
            .border_radius(size / 2.0)
            .into();
    }
    initials_circle(
        &app.theme.palette,
        user.display_name(),
        theme::default_avatar_color(user.id.0),
        size,
    )
}

pub fn initials_circle<'a>(p: &Palette, name: &str, color: iced::Color, size: f32) -> El<'a> {
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    container(
        text(initial)
            .size(size * 0.45)
            .font(INTER_SEMIBOLD)
            .color(p.accent_text),
    )
    .width(size)
    .height(size)
    .center_x(size)
    .center_y(size)
    .style(theme::rounded_bg(color, size / 2.0))
    .into()
}

/// Avatar with its presence dot (user panel and DM list).
pub fn avatar_with_status<'a>(app: &'a App, user: &User, size: f32) -> El<'a> {
    avatar_with_presence(
        app,
        user,
        size,
        app.chat.presence_of(user.id),
        app.theme.palette.user_panel,
    )
}

/// Avatar with a presence dot; `ring` is the background colour the dot is
/// cut out of.
pub fn avatar_with_presence<'a>(
    app: &'a App,
    user: &User,
    size: f32,
    status: crate::net::model::Presence,
    ring_color: iced::Color,
) -> El<'a> {
    let p = app.theme.palette;
    let dot = size * 0.3;
    let ring = container(
        container(text(""))
            .width(dot)
            .height(dot)
            .style(theme::rounded_bg(
                theme::presence_color(&p, status),
                dot / 2.0,
            )),
    )
    .padding(2)
    .style(theme::rounded_bg(ring_color, dot));
    stack![
        avatar(app, user, size),
        container(ring)
            .width(size + 2.0)
            .height(size + 2.0)
            .align_right(size + 2.0)
            .align_bottom(size + 2.0),
    ]
    .into()
}

/// Red pill with a mention count.
pub fn mention_badge<'a>(p: &Palette, count: u32, border: iced::Color) -> El<'a> {
    let p = *p;
    let label = if count > 99 {
        "99+".to_string()
    } else {
        count.to_string()
    };
    container(
        text(label)
            .size(12)
            .font(theme::INTER_BOLD)
            .color(p.accent_text),
    )
    .padding(Padding::from([0, 5]))
    .height(16)
    .center_y(16)
    .style(move |_| container::Style {
        background: Some(iced::Background::Color(p.badge)),
        border: iced::Border {
            radius: 8.0.into(),
            width: 0.0,
            color: border,
        },
        ..Default::default()
    })
    .into()
}

/// Fits (w, h) into (max_w, max_h) preserving aspect ratio.
pub fn fit_size(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (max_w.min(400), max_h.min(300));
    }
    let scale = (max_w as f32 / w as f32)
        .min(max_h as f32 / h as f32)
        .min(1.0);
    (
        ((w as f32 * scale).round() as u32).max(1),
        ((h as f32 * scale).round() as u32).max(1),
    )
}

/// "Today at 2:03 PM", "Yesterday at 9:15 AM" or "11/18/2026 2:03 PM".
pub fn format_timestamp(ms: u64, now_ms: u64) -> String {
    let Some(t) = local(ms) else {
        return String::new();
    };
    let now = local(now_ms).unwrap_or(t);
    let time = t.format("%-I:%M %p");
    let days = now.date_naive().num_days_from_ce() - t.date_naive().num_days_from_ce();
    match days {
        0 => format!("Today at {time}"),
        1 => format!("Yesterday at {time}"),
        _ => format!("{} {time}", t.format("%m/%d/%Y")),
    }
}

/// Short time for continuation messages ("2:03 PM").
pub fn format_time(ms: u64) -> String {
    local(ms)
        .map(|t| t.format("%-I:%M %p").to_string())
        .unwrap_or_default()
}

/// Day divider label ("November 18, 2026").
pub fn format_day(ms: u64) -> String {
    local(ms)
        .map(|t| t.format("%B %-d, %Y").to_string())
        .unwrap_or_default()
}

pub fn same_day(a_ms: u64, b_ms: u64) -> bool {
    match (local(a_ms), local(b_ms)) {
        (Some(a), Some(b)) => a.date_naive() == b.date_naive(),
        _ => true,
    }
}

/// `<t:unix:style>` formatting (Discord styles t T d D f F R).
pub fn format_discord_timestamp(unix: i64, style: char, now_ms: u64) -> String {
    let Some(t) = Local.timestamp_opt(unix, 0).single() else {
        return format!("<t:{unix}>");
    };
    match style {
        't' => t.format("%-I:%M %p").to_string(),
        'T' => t.format("%-I:%M:%S %p").to_string(),
        'd' => t.format("%m/%d/%Y").to_string(),
        'D' => t.format("%B %-d, %Y").to_string(),
        'F' => t.format("%A, %B %-d, %Y %-I:%M %p").to_string(),
        'R' => relative(unix * 1000 - now_ms as i64),
        _ => t.format("%B %-d, %Y %-I:%M %p").to_string(),
    }
}

fn relative(delta_ms: i64) -> String {
    let s = delta_ms.abs() / 1000;
    let (n, unit) = match s {
        0..=59 => (s, "second"),
        60..=3599 => (s / 60, "minute"),
        3600..=86_399 => (s / 3600, "hour"),
        86_400..=2_591_999 => (s / 86_400, "day"),
        2_592_000..=31_535_999 => (s / 2_592_000, "month"),
        _ => (s / 31_536_000, "year"),
    };
    let plural = if n == 1 { "" } else { "s" };
    if delta_ms >= 0 {
        format!("in {n} {unit}{plural}")
    } else {
        format!("{n} {unit}{plural} ago")
    }
}

fn local(ms: u64) -> Option<DateTime<Local>> {
    Local.timestamp_millis_opt(ms as i64).single()
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["bytes", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} bytes")
    } else {
        format!("{v:.2} {}", UNITS[i])
    }
}

/// Message shortcut for buttons.
pub fn ui(u: Ui) -> Message {
    Message::Ui(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit() {
        assert_eq!(fit_size(800, 450, 400, 300), (400, 225));
        assert_eq!(fit_size(100, 100, 400, 300), (100, 100));
        assert_eq!(fit_size(300, 1200, 400, 300), (75, 300));
    }

    #[test]
    fn timestamps() {
        let now = 1_700_000_000_000;
        assert!(format_timestamp(now, now).starts_with("Today at "));
        assert!(format_timestamp(now - 86_400_000, now).starts_with("Yesterday at "));
        assert!(format_timestamp(now - 10 * 86_400_000, now).contains('/'));
        assert_eq!(relative(-5 * 60_000), "5 minutes ago");
        assert_eq!(relative(3_600_000), "in 1 hour");
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(500), "500 bytes");
        assert_eq!(human_size(1_532_211), "1.46 MB");
    }
}
