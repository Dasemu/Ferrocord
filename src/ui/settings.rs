//! Settings dialog: tray and background-mode toggles.

use iced::widget::{Space, button, column, container, mouse_area, opaque, row, text, toggler};
use iced::{Alignment, Length, Padding};

use super::theme::{self, INTER_SEMIBOLD};
use super::{El, ui};
use crate::app::window::TrayHost;
use crate::app::{App, Ui};
use crate::config::APP_NAME;
use crate::tray::autostart;

pub fn view(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let tray_missing = app.cfg.tray && app.win.host == TrayHost::Unavailable;
    let tray_note: Option<&'static str> = if !app.cfg.tray {
        Some("The system tray is turned off (--no-tray): closing the window quits.")
    } else if tray_missing {
        Some("No system tray was found on this desktop: closing the window quits.")
    } else {
        None
    };
    let mut rows = column![
        text("Settings")
            .size(t.metrics.font_l)
            .font(INTER_SEMIBOLD)
            .color(p.header_text),
        text("Background")
            .size(t.metrics.font_s)
            .font(INTER_SEMIBOLD)
            .color(p.text_secondary),
        toggle_row(
            app,
            "Close button minimizes to tray",
            "Keep running in the background so notifications keep arriving.",
            app.prefs.close_to_tray,
            Ui::ToggleCloseToTray,
        ),
        toggle_row(
            app,
            "Start minimized to tray",
            "Launch without opening a window; click the tray icon to show it.",
            app.prefs.start_minimized,
            Ui::ToggleStartMinimized,
        ),
    ]
    .spacing(14);
    if autostart::SUPPORTED {
        rows = rows.push(toggle_row(
            app,
            &format!("Open {APP_NAME} when I log in"),
            "Adds an autostart entry that starts minimized to the tray.",
            app.autostart,
            Ui::ToggleAutostart,
        ));
    }
    rows = rows
        .push(
            text("Appearance")
                .size(t.metrics.font_s)
                .font(INTER_SEMIBOLD)
                .color(p.text_secondary),
        )
        .push(toggle_row(
            app,
            "Reduce motion",
            "Keep animated avatars, decorations, emoji, banners and profile effects still.",
            app.prefs.reduce_motion,
            Ui::ToggleReduceMotion,
        ))
        .push(
            text("Notifications")
                .size(t.metrics.font_s)
                .font(INTER_SEMIBOLD)
                .color(p.text_secondary),
        )
        .push(toggle_row(
            app,
            "Mute notifications",
            "No desktop notifications; unread badges still update.",
            app.prefs.muted,
            Ui::ToggleMuted,
        ));
    if let Some(note) = tray_note {
        rows = rows.push(text(note).size(t.metrics.font_s).color(p.text_muted));
    }
    rows = rows.push(super::voice::settings_section(app));
    rows = rows
        .push(
            text("Account")
                .size(t.metrics.font_s)
                .font(INTER_SEMIBOLD)
                .color(p.text_secondary),
        )
        .push(
            row![
                column![
                    text("Log out").size(t.metrics.font_m).color(p.text_normal),
                    text("Removes the saved session from this computer.")
                        .size(t.metrics.font_s)
                        .color(p.text_muted),
                ]
                .spacing(2)
                .width(Length::Fill),
                button(text("Log Out").size(14).color(p.accent_text))
                    .padding(Padding::from([8, 16]))
                    .on_press(ui(Ui::Logout))
                    .style(theme::danger_button(p)),
            ]
            .spacing(16)
            .align_y(Alignment::Center),
        );
    let card = container(
        column![
            rows,
            row![
                Space::new().width(Length::Fill),
                button(text("Done").size(14).color(p.accent_text))
                    .padding(Padding::from([8, 16]))
                    .on_press(ui(Ui::CloseSettings))
                    .style(theme::primary_button(p)),
            ],
        ]
        .spacing(18),
    )
    .width(460)
    .padding(20)
    .style(theme::rounded_bg(p.bg_panel, 8.0));
    opaque(
        mouse_area(
            container(opaque(card))
                .center(Length::Fill)
                .style(theme::bg(iced::Color {
                    a: 0.7,
                    ..iced::Color::BLACK
                })),
        )
        .on_press(ui(Ui::CloseSettings)),
    )
}

fn toggle_row<'a>(app: &'a App, title: &str, hint: &'a str, on: bool, msg: Ui) -> El<'a> {
    let t = &app.theme;
    let p = t.palette;
    row![
        column![
            text(title.to_owned())
                .size(t.metrics.font_m)
                .color(p.text_normal),
            text(hint).size(t.metrics.font_s).color(p.text_muted),
        ]
        .spacing(2)
        .width(Length::Fill),
        toggler(on)
            .on_toggle(move |_| ui(msg.clone()))
            .size(22.0)
            .style(theme::switch(p)),
    ]
    .spacing(16)
    .align_y(Alignment::Center)
    .into()
}
