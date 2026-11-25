//! The update banner and Settings → Updates.

use iced::widget::{Space, button, column, container, row, text};
use iced::{Alignment, Element, Length, Padding};

use super::{Banner, CheckStatus, InstallMethod, Phase, UpdateMsg};
use crate::app::{App, Message};
use crate::config::APP_NAME;
use crate::ui::settings::{heading, note, page_col, title, toggle};
use crate::ui::theme::{self, INTER_SEMIBOLD};

type El<'a> = Element<'a, Message>;

fn msg(m: UpdateMsg) -> Message {
    Message::Update(m)
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_000_000.0)
}

/// "Downloading… 45% (12.3 MB of 27.0 MB)".
pub fn progress_text(phase: &Phase) -> String {
    match phase {
        Phase::Downloading {
            received,
            total: Some(t),
        } if *t > 0 => format!(
            "Downloading… {}% ({} of {})",
            (received.saturating_mul(100) / t).min(100),
            mb(*received),
            mb(*t)
        ),
        Phase::Downloading { received, .. } => format!("Downloading… {}", mb(*received)),
        Phase::Verifying => "Verifying the checksum…".to_owned(),
        Phase::Installing => "Installing…".to_owned(),
        Phase::Checking { .. } => "Checking for updates…".to_owned(),
        _ => String::new(),
    }
}

/// The slim bar at the top of the main window, when there is something
/// to say. Never modal: every state can be closed or ignored.
pub fn banner(app: &App) -> Option<El<'_>> {
    let b = app.updater.banner()?;
    let t = &app.theme;
    let p = t.palette;
    let label = |s: String| text(s).size(t.metrics.font_s).color(p.accent_text);
    let btn = |s: &'static str, m: UpdateMsg| -> El<'_> {
        button(text(s).size(t.metrics.font_s).font(INTER_SEMIBOLD))
            .padding(Padding::from(theme::NOTICE_BUTTON_PAD))
            .on_press(msg(m))
            .style(theme::notice_button(p))
            .into()
    };
    let close = || -> El<'_> {
        button(crate::ui::icons::icon(
            crate::ui::icons::Icon::X,
            14.0,
            p.accent_text,
        ))
        .padding(Padding::from([2, 4]))
        .on_press(msg(UpdateMsg::Dismiss))
        .style(theme::link_button(p))
        .into()
    };
    let mut r = row![].spacing(10).align_y(Alignment::Center);
    let color = match &b {
        Banner::Available {
            version,
            self_update,
            cargo,
        } => {
            r = r.push(label(format!("{APP_NAME} {version} is available")));
            if *self_update {
                r = r.push(btn("Update", UpdateMsg::Start));
            } else if *cargo {
                r = r.push(label("Installed with cargo: run".to_owned()));
                r = r.push(
                    text(super::method::cargo_command())
                        .size(t.metrics.font_s)
                        .font(iced::Font::MONOSPACE)
                        .color(p.accent_text),
                );
                r = r.push(btn("Copy", UpdateMsg::CopyCommand));
            }
            r = r.push(btn("Release notes", UpdateMsg::OpenReleasePage));
            r = r.push(close());
            p.accent
        }
        Banner::Progress(phase) => {
            r = r.push(label(progress_text(phase)));
            if matches!(phase, Phase::Downloading { .. }) {
                r = r.push(btn("Cancel", UpdateMsg::Cancel));
            }
            p.accent
        }
        Banner::Ready(v) => {
            r = r.push(label(format!(
                "{APP_NAME} {v} is installed. Restart to finish."
            )));
            r = r.push(btn("Restart now", UpdateMsg::Restart));
            r = r.push(btn("Later", UpdateMsg::Later));
            p.success
        }
        Banner::Failed(e) => {
            r = r.push(label(format!("Update failed: {e}")));
            r = r.push(btn("Open release page", UpdateMsg::OpenReleasePage));
            r = r.push(close());
            p.danger
        }
    };
    Some(
        container(r)
            .width(Length::Fill)
            .padding(Padding::from([4, 16]))
            .align_x(Alignment::Center)
            .style(theme::bg(color))
            .into(),
    )
}

/// Settings → Updates.
pub fn page(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let u = &app.updater;
    let primary = |s: &'static str, m: Option<UpdateMsg>| -> El<'_> {
        let mut b = button(text(s).size(t.metrics.font_m))
            .padding(Padding::from([8, 16]))
            .style(theme::primary_button(p));
        if let Some(m) = m {
            b = b.on_press(msg(m));
        }
        b.into()
    };
    let secondary = |s: &'static str, m: UpdateMsg| -> El<'_> {
        button(text(s).size(t.metrics.font_m))
            .padding(Padding::from([8, 16]))
            .on_press(msg(m))
            .style(theme::secondary_button(p))
            .into()
    };
    let value = |label: &'static str, v: String| -> El<'_> {
        column![
            text(label)
                .size(t.metrics.font_xs)
                .font(INTER_SEMIBOLD)
                .color(p.text_secondary),
            text(v).size(t.metrics.font_m).color(p.text_normal),
        ]
        .spacing(4)
        .into()
    };

    let mut col = page_col().push(title(app, "Updates")).push(value(
        "CURRENT VERSION",
        format!("{APP_NAME} {}", u.current),
    ));
    let method_text = match (&u.method, u.allowed) {
        (Some(m), _) => m.label(),
        (None, false) => "Not checked in this mode".to_owned(),
        (None, true) => "Detecting…".to_owned(),
    };
    col = col.push(value("INSTALLED WITH", method_text));
    if let Some(m) = &u.method {
        col = col.push(note(app, m.explanation()));
    }

    // Check button and its result.
    let checking = matches!(u.phase, Phase::Checking { .. });
    let status = if checking {
        progress_text(&u.phase)
    } else {
        match &u.status {
            CheckStatus::Never => String::new(),
            CheckStatus::UpToDate => format!("{APP_NAME} is up to date."),
            CheckStatus::Available(v) => format!("{APP_NAME} {v} is available."),
            CheckStatus::Off => "Update checks are off in mock and screenshot mode.".to_owned(),
            CheckStatus::Error(e) => format!("Could not check: {e}"),
        }
    };
    let status_color = match u.status {
        CheckStatus::Error(_) if !checking => p.danger,
        _ => p.text_secondary,
    };
    let check_btn = primary(
        "Check for updates",
        (!u.phase.busy()).then_some(UpdateMsg::CheckNow),
    );
    col = col.push(
        row![
            check_btn,
            text(status).size(t.metrics.font_s).color(status_color)
        ]
        .spacing(16)
        .align_y(Alignment::Center),
    );

    // What can be done with an available version.
    if let Some(r) = u.available() {
        let mut actions = row![].spacing(12).align_y(Alignment::Center);
        match (&u.phase, &u.method) {
            (ph, _) if ph.installing() => {
                actions = actions.push(
                    text(progress_text(ph))
                        .size(t.metrics.font_m)
                        .color(p.text_normal),
                );
                if matches!(ph, Phase::Downloading { .. }) {
                    actions = actions.push(secondary("Cancel", UpdateMsg::Cancel));
                }
            }
            (Phase::Ready { version }, _) => {
                actions = actions
                    .push(
                        text(format!(
                            "{APP_NAME} {version} is installed. Restart to finish."
                        ))
                        .size(t.metrics.font_m)
                        .color(p.text_normal),
                    )
                    .push(primary("Restart now", Some(UpdateMsg::Restart)));
            }
            (_, Some(m)) if m.can_self_update() => {
                actions = actions.push(primary("Update now", Some(UpdateMsg::Start)));
            }
            (_, Some(InstallMethod::Cargo)) => {
                actions = actions
                    .push(
                        container(
                            text(super::method::cargo_command())
                                .size(t.metrics.font_s)
                                .font(iced::Font::MONOSPACE)
                                .color(p.text_normal),
                        )
                        .padding(8)
                        .style(theme::rounded_bg(p.code_bg, t.metrics.radius)),
                    )
                    .push(secondary("Copy", UpdateMsg::CopyCommand));
            }
            _ => {
                actions = actions.push(secondary("Open release page", UpdateMsg::OpenReleasePage));
            }
        }
        col = col
            .push(heading(app, "NEW VERSION"))
            .push(value("AVAILABLE", format!("{APP_NAME} {}", r.version)))
            .push(actions);
    }
    if let Phase::Failed(e) = &u.phase {
        col = col.push(
            row![
                text(format!("Update failed: {e}"))
                    .size(t.metrics.font_s)
                    .color(p.danger)
                    .width(Length::Fill),
                secondary("Open release page", UpdateMsg::OpenReleasePage),
            ]
            .spacing(12)
            .align_y(Alignment::Center),
        );
    }

    col = col
        .push(heading(app, "AUTOMATIC CHECKS"))
        .push(toggle(
            app,
            "Check for updates automatically",
            "Asks GitHub shortly after start and every 6 hours, also while in the tray. Manual checks always work.",
            u.auto,
            msg(UpdateMsg::ToggleAuto),
        ))
        .push(
            row![
                button(text("Release notes").size(t.metrics.font_s).color(p.link))
                    .padding(0)
                    .on_press(msg(UpdateMsg::OpenReleasePage))
                    .style(theme::link_button(p)),
                Space::new().width(Length::Fill),
            ],
        )
        .push(note(
            app,
            "Downloads come from GitHub over HTTPS and are checked against the release's SHA-256 checksum. \
             Releases are not signed yet, so the checksum proves the file arrived intact, not who built it.",
        ));
    col.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_texts() {
        assert_eq!(
            progress_text(&Phase::Downloading {
                received: 13_500_000,
                total: Some(27_000_000)
            }),
            "Downloading… 50% (13.5 MB of 27.0 MB)"
        );
        assert_eq!(
            progress_text(&Phase::Downloading {
                received: 1_000_000,
                total: None
            }),
            "Downloading… 1.0 MB"
        );
        assert_eq!(progress_text(&Phase::Verifying), "Verifying the checksum…");
        assert_eq!(progress_text(&Phase::Idle), "");
    }
}
