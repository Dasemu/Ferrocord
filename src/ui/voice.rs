//! Voice views: users under voice channels, the "Voice Connected" panel,
//! the DM call area, the incoming call overlay, the per-user volume menu,
//! call service messages and the voice settings section.

use iced::widget::{
    Space, button, column, container, mouse_area, opaque, pick_list, row, slider, stack, text,
};
use iced::{Alignment, Color, Length, Padding, Point, Size};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, avatar, initials_circle, ui};
use crate::app::voice::{Phase, VoiceUi};
use crate::app::{App, Ui, input};
use crate::net::model::*;

const USER_MENU_W: f32 = 220.0;
const USER_MENU_H: f32 = 132.0;

fn vui(v: VoiceUi) -> crate::app::Message {
    ui(Ui::Voice(v))
}

/// Avatar with the green speaking ring of the official client.
pub fn speaking_avatar<'a>(app: &'a App, user: &User, size: f32, speaking: bool) -> El<'a> {
    let p = app.theme.palette;
    let ring = if speaking {
        p.success
    } else {
        Color::TRANSPARENT
    };
    container(avatar(app, user, size))
        .padding(2)
        .style(move |_| container::Style {
            border: iced::Border {
                color: ring,
                width: 2.0,
                radius: ((size + 4.0) / 2.0).into(),
            },
            ..Default::default()
        })
        .into()
}

fn member_user(app: &App, v: &VoiceState) -> User {
    app.chat
        .users
        .get(&v.user_id)
        .cloned()
        .or_else(|| v.user.clone())
        .unwrap_or(User {
            id: v.user_id,
            username: v.user_id.to_string(),
            global_name: None,
            avatar: None,
            bot: false,
        })
}

/// Mute / deafen glyphs after a name (server state red, self state grey).
fn state_icons<'a>(app: &App, v: &VoiceState) -> El<'a> {
    let p = app.theme.palette;
    let mut r = row![].spacing(4).align_y(Alignment::Center);
    if v.mute || v.self_mute {
        r = r.push(icon(
            Icon::MicOff,
            14.0,
            if v.mute { p.danger } else { p.text_muted },
        ));
    }
    if v.deaf || v.self_deaf {
        r = r.push(icon(
            Icon::HeadphoneOff,
            14.0,
            if v.deaf { p.danger } else { p.text_muted },
        ));
    }
    if app.voice.user_volume(v.user_id).1 {
        r = r.push(icon(Icon::VolumeX, 14.0, p.danger));
    }
    r.into()
}

/// Users connected to a voice channel, listed under it in the sidebar.
pub fn channel_members<'a>(app: &'a App, channel: ChannelId) -> Option<El<'a>> {
    let p = app.theme.palette;
    let members = app.voice.channel_members(channel);
    if members.is_empty() {
        return None;
    }
    let mut col = column![].spacing(2);
    for v in members {
        let u = member_user(app, v);
        let speaking = app.voice.is_speaking(u.id);
        let name = u.display_name().to_owned();
        let row_el = row![
            speaking_avatar(app, &u, 20.0, speaking),
            text(super::ellipsize(&name, 120.0, 14.0, false))
                .size(14)
                .font(INTER_MEDIUM)
                .color(if speaking {
                    p.header_text
                } else {
                    p.channel_icon
                })
                .wrapping(text::Wrapping::None)
                .width(Length::Fill),
            state_icons(app, v),
        ]
        .spacing(8)
        .align_y(Alignment::Center);
        let b = button(row_el)
            .padding(Padding::from([3, 8]))
            .width(Length::Fill)
            .style(theme::list_row(p, false));
        col = col.push(mouse_area(b).on_right_press(vui(VoiceUi::UserMenu(u.id))));
    }
    Some(
        container(col)
            .padding(Padding::ZERO.left(36).right(8).bottom(4))
            .into(),
    )
}

fn location(app: &App) -> Option<(String, String)> {
    let c = app.voice.conn.as_ref()?;
    let ch = app.chat.channels.get(&c.channel)?;
    let name = match ch.kind {
        ChannelKind::Dm | ChannelKind::GroupDm => ch.label(|u| app.chat.user_name(u)),
        _ => ch.name.clone().unwrap_or_default(),
    };
    let place = match c.guild.and_then(|g| app.chat.guild(g)) {
        Some(g) => format!("{name} / {}", g.name),
        None => name,
    };
    Some((place, String::new()))
}

/// Connection quality from the voice gateway round trip.
pub fn quality(ping: Option<u32>) -> (&'static str, Quality) {
    match ping {
        None => ("Connecting", Quality::Pending),
        Some(ms) if ms < 150 => ("Voice Connected", Quality::Good),
        Some(ms) if ms < 400 => ("Voice Connected", Quality::Fair),
        Some(_) => ("Voice Connected", Quality::Poor),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Pending,
    Good,
    Fair,
    Poor,
}

/// The "Voice Connected" panel above the user panel.
pub fn connected_panel(app: &App) -> Option<El<'_>> {
    let c = app.voice.conn.as_ref()?;
    let t = &app.theme;
    let p = t.palette;
    let (place, _) = location(app)?;
    let (label, q) = match c.phase {
        Phase::Requesting | Phase::Connecting => ("RTC Connecting", Quality::Pending),
        Phase::Reconnecting => ("Reconnecting", Quality::Pending),
        Phase::Connected => quality(c.ping),
    };
    let color = match q {
        Quality::Good => p.success,
        Quality::Fair | Quality::Pending => p.status_idle,
        Quality::Poor => p.danger,
    };
    let mut status = row![
        icon(Icon::Signal, 16.0, color),
        text(label).size(14).font(INTER_SEMIBOLD).color(color),
    ]
    .spacing(6)
    .align_y(Alignment::Center);
    if let Some(ms) = c.ping.filter(|_| c.phase == Phase::Connected) {
        status = status.push(text(format!("{ms} ms")).size(12).color(p.text_muted));
    }
    let mut sub = row![].spacing(4).align_y(Alignment::Center);
    if c.e2ee {
        sub = sub.push(icon(Icon::ShieldCheck, 12.0, p.success));
    }
    sub = sub.push(
        text(super::ellipsize(&place, 150.0, 12.0, false))
            .size(12)
            .color(p.text_secondary)
            .wrapping(text::Wrapping::None),
    );
    let leave = button(container(icon(Icon::PhoneOff, 20.0, p.text_secondary)).center(32))
        .width(32)
        .height(32)
        .padding(0)
        .on_press(vui(VoiceUi::Leave))
        .style(theme::icon_button(p));
    let tip = iced::widget::tooltip(
        leave,
        container(text("Disconnect").size(13).color(p.header_text))
            .padding(Padding::from([6, 10]))
            .style(theme::rounded_bg(p.bg_elevated, 5.0)),
        iced::widget::tooltip::Position::Top,
    )
    .gap(6);
    Some(
        container(
            row![column![status, sub].spacing(2).width(Length::Fill), tip]
                .align_y(Alignment::Center),
        )
        .padding(Padding::from([8, 8]).left(12))
        .width(Length::Fill)
        .style(theme::bg(p.user_panel))
        .into(),
    )
}

fn round_button<'a>(app: &App, i: Icon, bg: Color, on: crate::app::Message, size: f32) -> El<'a> {
    let p = app.theme.palette;
    button(container(icon(i, size * 0.42, p.header_text)).center(size))
        .width(size)
        .height(size)
        .padding(0)
        .on_press(on)
        .style(move |_, status| {
            let hover = matches!(status, button::Status::Hovered | button::Status::Pressed);
            button::Style {
                background: Some(iced::Background::Color(if hover {
                    Color { a: 0.85, ..bg }
                } else {
                    bg
                })),
                border: iced::Border {
                    radius: (size / 2.0).into(),
                    ..Default::default()
                },
                ..Default::default()
            }
        })
        .into()
}

/// The call area shown at the top of a DM while a call is going on.
pub fn dm_call_area<'a>(app: &'a App, c: &'a Channel) -> Option<El<'a>> {
    let joined = app.voice.connected_channel() == Some(c.id);
    if !joined && !app.voice.active_call(c.id) {
        return None;
    }
    let p = app.theme.palette;
    let members = app.voice.channel_members(c.id);
    let mut faces = row![].spacing(16).align_y(Alignment::Center);
    for v in &members {
        let u = member_user(app, v);
        let speaking = app.voice.is_speaking(u.id);
        let mut face = column![speaking_avatar(app, &u, 80.0, speaking)]
            .spacing(6)
            .align_x(Alignment::Center);
        let state = state_icons(app, v);
        face = face.push(state);
        faces = faces.push(mouse_area(face).on_right_press(vui(VoiceUi::UserMenu(u.id))));
    }
    // People being rung show greyed out.
    if let Some(call) = app.voice.calls.get(&c.id) {
        for uid in &call.ringing {
            if members.iter().any(|m| m.user_id == *uid) {
                continue;
            }
            if let Some(u) = app.chat.users.get(uid) {
                faces = faces.push(
                    container(avatar(app, u, 80.0))
                        .padding(2)
                        .style(theme::rounded_bg(Color { a: 0.4, ..p.bg_app }, 42.0)),
                );
            }
        }
    }
    let controls: El<'a> = if joined {
        row![
            round_button(
                app,
                if app.voice.self_mute || app.voice.self_deaf {
                    Icon::MicOff
                } else {
                    Icon::Mic
                },
                p.bg_elevated,
                vui(VoiceUi::ToggleMute),
                48.0
            ),
            round_button(
                app,
                if app.voice.self_deaf {
                    Icon::HeadphoneOff
                } else {
                    Icon::Headphones
                },
                p.bg_elevated,
                vui(VoiceUi::ToggleDeafen),
                48.0
            ),
            round_button(app, Icon::PhoneOff, p.danger, vui(VoiceUi::Leave), 48.0),
        ]
        .spacing(16)
        .into()
    } else {
        button(
            row![
                icon(Icon::Phone, 16.0, p.header_text),
                text("Join Call")
                    .size(14)
                    .font(INTER_MEDIUM)
                    .color(p.header_text)
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        )
        .padding(Padding::from([8, 16]))
        .on_press(vui(VoiceUi::StartCall(c.id)))
        .style(move |_, _| button::Style {
            background: Some(iced::Background::Color(p.success)),
            border: iced::Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            ..Default::default()
        })
        .into()
    };
    Some(
        container(
            column![faces, controls]
                .spacing(24)
                .align_x(Alignment::Center),
        )
        .width(Length::Fill)
        .padding(Padding::from([32, 16]))
        .center_x(Length::Fill)
        .style(theme::bg(p.bg_app))
        .into(),
    )
}

/// Full-window overlay for an incoming call (Join / Decline).
pub fn incoming_overlay(app: &App) -> Option<El<'_>> {
    let ch = app.voice.incoming?;
    let c = app.chat.channels.get(&ch)?;
    let p = app.theme.palette;
    let name = c.label(|u| app.chat.user_name(u));
    let pic: El<'_> = match c.recipients.first().and_then(|u| app.chat.users.get(u)) {
        Some(u) if c.kind == ChannelKind::Dm => avatar(app, u, 80.0),
        _ => initials_circle(&p, &name, theme::default_avatar_color(c.id.0), 80.0),
    };
    let card = container(
        column![
            pic,
            text(super::ellipsize(&name, 200.0, 18.0, true))
                .size(18)
                .font(INTER_SEMIBOLD)
                .color(p.header_text),
            text("Incoming Call…").size(14).color(p.text_secondary),
            row![
                round_button(
                    app,
                    Icon::PhoneOff,
                    p.danger,
                    vui(VoiceUi::DeclineCall(ch)),
                    56.0
                ),
                round_button(
                    app,
                    Icon::Phone,
                    p.success,
                    vui(VoiceUi::AcceptCall(ch)),
                    56.0
                ),
            ]
            .spacing(32),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .width(232)
    .padding(Padding::from([24, 16]))
    .style(theme::rounded_bg(p.bg_app, 8.0));
    Some(
        container(opaque(card))
            .center(Length::Fill)
            .style(theme::bg(Color {
                a: 0.6,
                ..Color::BLACK
            }))
            .into(),
    )
}

/// Right-click menu on a user in voice: volume slider and local mute.
pub fn user_menu(app: &App) -> Option<El<'_>> {
    let m = app.voice.user_menu?;
    let p = app.theme.palette;
    let me = app.chat.me_id();
    let (vol, muted) = app.voice.user_volume(m.user);
    let name = app.chat.user_name(m.user).unwrap_or("Unknown").to_owned();
    let card: El<'_> = if me == Some(m.user) {
        container(
            text("Your own volume is set in Voice settings.")
                .size(13)
                .color(p.text_secondary),
        )
        .width(USER_MENU_W)
        .padding(12)
        .style(theme::popup(p))
        .into()
    } else {
        let user = m.user;
        container(
            column![
                text(super::ellipsize(&name, USER_MENU_W - 24.0, 14.0, true))
                    .size(14)
                    .font(INTER_SEMIBOLD)
                    .color(p.header_text),
                row![
                    text("User Volume")
                        .size(12)
                        .color(p.text_muted)
                        .width(Length::Fill),
                    text(format!("{vol:.0}%")).size(12).color(p.text_muted),
                ],
                slider(0.0..=200.0, vol, move |v| vui(VoiceUi::UserVolume(user, v))).step(1.0_f32),
                button(
                    row![
                        text("Mute")
                            .size(14)
                            .color(p.text_normal)
                            .width(Length::Fill),
                        icon(
                            if muted { Icon::Check } else { Icon::VolumeX },
                            16.0,
                            if muted { p.accent } else { p.text_muted }
                        ),
                    ]
                    .align_y(Alignment::Center),
                )
                .padding(Padding::from([6, 8]))
                .width(Length::Fill)
                .on_press(vui(VoiceUi::ToggleUserMute(user)))
                .style(theme::menu_item(p, false)),
            ]
            .spacing(8),
        )
        .width(USER_MENU_W)
        .padding(12)
        .style(theme::popup(p))
        .into()
    };
    let at = input::clamp_popup(
        m.at,
        Size::new(USER_MENU_W, USER_MENU_H),
        input::window_size(),
    );
    Some(super::popups::popup_layer(
        app,
        card,
        Point::new(at.x, at.y),
        Ui::Voice(VoiceUi::CloseUserMenu),
    ))
}

/// Call service message ("X started a call that lasted 5 minutes").
pub fn call_message<'a>(app: &'a App, m: &'a Message, now: u64) -> El<'a> {
    let p = app.theme.palette;
    let call = m.call.clone().unwrap_or_default();
    let me = app.chat.me_id();
    let who = if Some(m.author.id) == me {
        "You".to_owned()
    } else {
        m.author.display_name().to_owned()
    };
    let missed = me.is_some_and(|me| me != m.author.id && !call.participants.contains(&me));
    let line = match call.ended_ms {
        None => format!("{who} started a call."),
        Some(end) if missed => {
            let _ = end;
            format!("You missed a call from {who}.")
        }
        Some(end) => {
            let mins = end.saturating_sub(m.timestamp_ms()) / 60_000;
            let dur = match mins {
                0 => "a few seconds".to_owned(),
                1 => "a minute".to_owned(),
                n if n < 60 => format!("{n} minutes"),
                n => format!("{} hours", n / 60),
            };
            format!("{who} started a call that lasted {dur}.")
        }
    };
    let color = if missed && call.ended_ms.is_some() {
        p.danger
    } else {
        p.success
    };
    let mut r = row![
        container(icon(Icon::Phone, 16.0, color))
            .width(56)
            .align_x(Alignment::Center),
        text(line).size(15).color(p.text_secondary),
        text(super::format_timestamp(m.timestamp_ms(), now))
            .size(12)
            .color(p.text_muted),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    if call.ended_ms.is_none()
        && app.voice.connected_channel() != Some(m.channel_id)
        && app.voice.active_call(m.channel_id)
    {
        r = r.push(
            button(text("Join Call").size(13).color(p.accent_text))
                .padding(Padding::from([4, 10]))
                .on_press(vui(VoiceUi::StartCall(m.channel_id)))
                .style(theme::primary_button(p)),
        );
    }
    container(r)
        .padding(Padding::from([8, 0]).left(16))
        .width(Length::Fill)
        .into()
}

/// Voice settings section (devices, input sensitivity with a live meter).
pub fn settings_section(app: &App) -> El<'_> {
    let t = &app.theme;
    let p = t.palette;
    let default = "Default".to_owned();
    let with_default = |v: &[String]| -> Vec<String> {
        std::iter::once(default.clone())
            .chain(v.iter().cloned())
            .collect()
    };
    let inputs = with_default(&app.voice.devices.0);
    let outputs = with_default(&app.voice.devices.1);
    let sel_in = app
        .voice
        .prefs
        .input
        .clone()
        .unwrap_or_else(|| default.clone());
    let sel_out = app
        .voice
        .prefs
        .output
        .clone()
        .unwrap_or_else(|| default.clone());
    let pick = |s: String| if s == "Default" { None } else { Some(s) };
    let threshold = app.voice.prefs.threshold_db;
    // The microphone is only open during a call.
    let level = if app.voice.conn.is_some() {
        app.voice.control.input_level()
    } else {
        crate::net::voice::mixer::FLOOR_DB
    };
    // Meter: 0..1 over -100..0 dB; green when above the threshold.
    let frac = ((level + 100.0) / 100.0).clamp(0.0, 1.0);
    let meter_color = if level >= threshold {
        p.success
    } else {
        p.status_idle
    };
    let filled = ((frac * 1000.0) as u16).max(1);
    let meter = stack![
        container(Space::new().width(Length::Fill).height(6))
            .style(theme::rounded_bg(p.bg_app, 3.0)),
        row![
            container(Space::new().width(Length::Fill).height(6))
                .width(Length::FillPortion(filled))
                .style(theme::rounded_bg(meter_color, 3.0)),
            Space::new()
                .width(Length::FillPortion(1000u16.saturating_sub(filled).max(1)))
                .height(6),
        ],
    ];
    let label = |s: &'static str| text(s).size(12).font(INTER_SEMIBOLD).color(p.text_muted);
    column![
        text("Voice")
            .size(t.metrics.font_s)
            .font(INTER_SEMIBOLD)
            .color(p.text_secondary),
        row![
            column![
                label("INPUT DEVICE"),
                pick_list(inputs, Some(sel_in), move |s| vui(VoiceUi::InputDevice(
                    pick(s)
                )))
                .width(Length::Fill),
            ]
            .spacing(6)
            .width(Length::Fill),
            column![
                label("OUTPUT DEVICE"),
                pick_list(outputs, Some(sel_out), move |s| vui(VoiceUi::OutputDevice(
                    pick(s)
                )))
                .width(Length::Fill),
            ]
            .spacing(6)
            .width(Length::Fill),
        ]
        .spacing(16),
        row![
            label("INPUT SENSITIVITY").width(Length::Fill),
            text(format!("{threshold:.0} dB"))
                .size(12)
                .color(p.text_muted),
        ],
        slider(-100.0..=0.0, threshold, |v| vui(VoiceUi::Threshold(v))).step(1.0_f32),
        meter,
        text("Your voice is sent when the bar is green.")
            .size(12)
            .color(p.text_muted),
    ]
    .spacing(10)
    .into()
}

/// Mic and headphone buttons for the user panel: (icon, tooltip, message,
/// active-red).
pub fn panel_buttons(app: &App) -> [(Icon, &'static str, Ui, bool); 2] {
    let v = &app.voice;
    let muted = v.self_mute || v.self_deaf;
    [
        (
            if muted { Icon::MicOff } else { Icon::Mic },
            if muted { "Unmute" } else { "Mute" },
            Ui::Voice(VoiceUi::ToggleMute),
            muted,
        ),
        (
            if v.self_deaf {
                Icon::HeadphoneOff
            } else {
                Icon::Headphones
            },
            if v.self_deaf { "Undeafen" } else { "Deafen" },
            Ui::Voice(VoiceUi::ToggleDeafen),
            v.self_deaf,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quality_thresholds() {
        assert_eq!(quality(None).1, Quality::Pending);
        assert_eq!(quality(Some(40)).1, Quality::Good);
        assert_eq!(quality(Some(200)).1, Quality::Fair);
        assert_eq!(quality(Some(900)).1, Quality::Poor);
    }
}
