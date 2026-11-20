//! The video player: the inline box in a message (poster, play button,
//! controls) and the full-screen view.
//!
//! The controls are always laid out (nothing appears or disappears on
//! hover), so the row never changes size under the pointer. Every colour
//! comes from `ui::theme`.

use iced::widget::{
    Space, button, column, container, image, opaque, progress_bar, row, slider, stack, text,
};
use iced::{Alignment, Background, Border, ContentFit, Length, Padding};

use super::icons::{Icon, icon};
use super::theme::{self, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, empty, fit_size, human_size, ui};
use crate::app::video::{Phase, Playback};
use crate::app::{App, Ui};
use crate::media::clip::Clip;
use crate::media::video::format_clock;

/// `1:05 / 3:20`, or just the position when the length is unknown.
pub fn time_label(pb: &Playback) -> String {
    let pos = format_clock(pb.progress_ms());
    if pb.duration_ms == 0 {
        pos
    } else {
        format!("{pos} / {}", format_clock(pb.duration_ms))
    }
}

/// Share of the download done, `0.0..=1.0` (0 when the size is unknown).
pub fn download_fraction(received: u64, total: Option<u64>) -> f32 {
    match total {
        Some(t) if t > 0 => (received as f64 / t as f64).clamp(0.0, 1.0) as f32,
        _ => 0.0,
    }
}

/// The inline player of `clip` inside a message.
pub fn inline<'a>(app: &'a App, clip: &Clip) -> El<'a> {
    let p = app.theme.palette;
    let active = app.playback_of(&clip.id);
    let (mut w, mut h) = clip.inline_size();
    if let Some(pb) = active
        && (clip.width.is_none() || clip.height.is_none())
        && pb.frame_size.0 > 0
    {
        (w, h) = fit_size(pb.frame_size.0, pb.frame_size.1, 400, 300);
    }
    let (wf, hf) = (w as f32, h as f32);
    let base = picture(app, clip, active, wf, hf);
    let layers: Vec<El<'a>> = match active {
        None => vec![
            base,
            click_area(
                Ui::PlayVideo(Box::new(clip.clone())),
                wf,
                hf,
                play_circle(app, Icon::Play),
            ),
            size_tag(clip, wf, hf),
        ],
        Some(pb) => match &pb.phase {
            Phase::Downloading { received, total } => {
                let label = match total {
                    Some(t) if *t > 0 => format!(
                        "Downloading {} of {}",
                        human_size(*received),
                        human_size(*t)
                    ),
                    _ => format!("Downloading {}", human_size(*received)),
                };
                vec![
                    base,
                    status_box(
                        app,
                        wf,
                        hf,
                        column![
                            text(label).size(13).color(theme::video_fg()),
                            progress_bar(0.0..=1.0, download_fraction(*received, *total))
                                .length(160)
                                .girth(4)
                                .style(move |_| progress_style(p)),
                        ]
                        .spacing(8)
                        .align_x(Alignment::Center)
                        .into(),
                    ),
                    corner_close(app, wf),
                ]
            }
            Phase::Starting => vec![
                base,
                status_box(
                    app,
                    wf,
                    hf,
                    text("Starting\u{2026}")
                        .size(13)
                        .color(theme::video_fg())
                        .into(),
                ),
                corner_close(app, wf),
            ],
            Phase::Failed { text: why } => vec![base, failure_box(app, clip, why, wf, hf)],
            Phase::Playing => {
                let mut v = vec![
                    base,
                    click_area(Ui::VideoToggle, wf, hf, Space::new().into()),
                ];
                if !pb.is_playing() {
                    v.push(indicator(
                        app,
                        wf,
                        hf,
                        if pb.is_ended() {
                            Icon::RotateCcw
                        } else {
                            Icon::Play
                        },
                    ));
                }
                v.push(
                    container(controls(app, pb, false))
                        .width(Length::Fixed(wf))
                        .height(Length::Fixed(hf))
                        .align_y(Alignment::End)
                        .into(),
                );
                v
            }
        },
    };
    container(stack(layers).width(wf).height(hf))
        .width(Length::Fixed(wf))
        .height(Length::Fixed(hf))
        .into()
}

/// The picture under everything: the live frame, else the poster, else a
/// dark box.
fn picture<'a>(app: &'a App, clip: &Clip, active: Option<&'a Playback>, w: f32, h: f32) -> El<'a> {
    let p = app.theme.palette;
    if let Some(handle) = active.and_then(|pb| pb.frame.as_ref()) {
        return container(
            image(handle.clone())
                .width(w)
                .height(h)
                .content_fit(ContentFit::Contain),
        )
        .width(w)
        .height(h)
        .center_x(w)
        .center_y(h)
        .style(theme::rounded_bg(theme::video_bg(), 8.0))
        .into();
    }
    if let Some(handle) = clip.poster.as_deref().and_then(|u| app.image(u)) {
        return image(handle.clone())
            .width(w)
            .height(h)
            .content_fit(ContentFit::Cover)
            .border_radius(8.0)
            .into();
    }
    container(Space::new())
        .width(w)
        .height(h)
        .style(theme::rounded_bg(p.embed_bg, 8.0))
        .into()
}

/// A transparent button over the whole picture.
fn click_area<'a>(on: Ui, w: f32, h: f32, content: El<'a>) -> El<'a> {
    button(content)
        .width(w)
        .height(h)
        .padding(0)
        .style(theme::video_surface())
        .on_press(ui(on))
        .into()
}

/// The round play / replay button, drawn (the click area under it handles
/// the press).
fn play_circle<'a>(app: &'a App, which: Icon) -> El<'a> {
    let p = app.theme.palette;
    container(icon(which, 28.0, theme::video_fg()))
        .padding(16)
        .style(move |_| container::Style {
            background: Some(Background::Color(theme::video_scrim())),
            border: Border {
                radius: 32.0.into(),
                ..Default::default()
            },
            text_color: Some(p.accent_text),
            ..Default::default()
        })
        .into()
}

fn indicator<'a>(app: &'a App, w: f32, h: f32, which: Icon) -> El<'a> {
    container(play_circle(app, which))
        .width(w)
        .height(h)
        .center_x(w)
        .center_y(h)
        .into()
}

/// Size of the file at the bottom left of an idle poster.
fn size_tag<'a>(clip: &Clip, w: f32, h: f32) -> El<'a> {
    if clip.size == 0 {
        return empty();
    }
    container(
        container(
            text(human_size(clip.size))
                .size(12)
                .font(INTER_MEDIUM)
                .color(theme::video_fg()),
        )
        .padding(Padding::from([2, 6]))
        .style(theme::rounded_bg(theme::video_scrim(), 4.0)),
    )
    .width(w)
    .height(h)
    .padding(8)
    .align_y(Alignment::End)
    .into()
}

fn status_box<'a>(_app: &'a App, w: f32, h: f32, content: El<'a>) -> El<'a> {
    container(content)
        .width(w)
        .height(h)
        .center_x(w)
        .center_y(h)
        .style(theme::rounded_bg(theme::video_scrim(), 8.0))
        .into()
}

fn corner_close<'a>(_app: &'a App, w: f32) -> El<'a> {
    container(
        button(icon(Icon::X, 16.0, theme::video_fg()))
            .padding(6)
            .style(theme::video_control(_app.theme.palette))
            .on_press(ui(Ui::CloseVideo)),
    )
    .width(w)
    .align_x(Alignment::End)
    .padding(6)
    .into()
}

fn failure_box<'a>(app: &'a App, clip: &Clip, why: &'a str, w: f32, h: f32) -> El<'a> {
    let p = app.theme.palette;
    let actions = row![
        button(
            row![
                icon(Icon::ExternalLink, 14.0, p.accent_text),
                text("Open externally").size(13).font(INTER_SEMIBOLD),
            ]
            .spacing(6)
            .align_y(Alignment::Center)
        )
        .padding(Padding::from([6, 12]))
        .style(theme::primary_button(p))
        .on_press(ui(Ui::OpenUrl(clip.url.clone()))),
        button(text("Close").size(13).font(INTER_SEMIBOLD))
            .padding(Padding::from([6, 12]))
            .style(theme::video_control(p))
            .on_press(ui(Ui::CloseVideo)),
    ]
    .spacing(8);
    status_box(
        app,
        w,
        h,
        column![
            icon(Icon::TriangleAlert, 22.0, p.danger),
            text(why)
                .size(13)
                .color(theme::video_fg())
                .width(Length::Fixed((w - 40.0).max(120.0)))
                .align_x(Alignment::Center),
            actions,
        ]
        .spacing(8)
        .align_x(Alignment::Center)
        .into(),
    )
}

fn progress_style(p: theme::Palette) -> progress_bar::Style {
    progress_bar::Style {
        background: Background::Color(theme::video_fg_dim()),
        bar: Background::Color(p.accent),
        border: Border {
            radius: 2.0.into(),
            ..Default::default()
        },
    }
}

/// Play / pause, time, seek bar, mute, volume, full screen.
fn controls<'a>(app: &'a App, pb: &'a Playback, full: bool) -> El<'a> {
    let p = app.theme.palette;
    let btn = |i: Icon, on: Ui| {
        button(icon(i, 20.0, theme::video_fg()))
            .padding(6)
            .style(theme::video_control(p))
            .on_press(ui(on))
    };
    let play = if pb.is_ended() {
        Icon::RotateCcw
    } else if pb.is_playing() {
        Icon::Pause
    } else {
        Icon::Play
    };
    let seek: El<'a> = if pb.duration_ms > 0 {
        slider(0.0..=pb.duration_ms as f32, pb.progress_ms() as f32, |v| {
            ui(Ui::VideoSeekDrag(v.max(0.0) as u64))
        })
        .on_release(ui(Ui::VideoSeekRelease))
        .height(16)
        .style(theme::video_slider(p))
        .width(Length::Fill)
        .into()
    } else {
        Space::new().width(Length::Fill).into()
    };
    let volume_icon = if pb.muted || pb.volume == 0 {
        Icon::VolumeX
    } else {
        Icon::Volume2
    };
    let shown_volume = if pb.muted { 0 } else { pb.volume };
    let bar = row![
        btn(play, Ui::VideoToggle),
        text(time_label(pb))
            .size(12)
            .font(INTER_MEDIUM)
            .color(theme::video_fg()),
        seek,
        btn(volume_icon, Ui::VideoMute),
        slider(0..=100u8, shown_volume, |v| ui(Ui::VideoVolume(v)))
            .height(16)
            .style(theme::video_slider(p))
            .width(if full { 110 } else { 56 }),
        btn(
            if full { Icon::Minimize } else { Icon::Maximize },
            Ui::VideoFullscreen
        ),
    ]
    .spacing(8)
    .align_y(Alignment::Center);
    container(bar)
        .width(Length::Fill)
        .padding(Padding::from([4, 8]))
        .style(theme::bg(theme::video_scrim()))
        .into()
}

/// The full-screen layer: black, the picture as large as fits, the title
/// and a close button on top, the controls at the bottom. `None` while the
/// player is not full screen.
pub fn fullscreen(app: &App) -> Option<El<'_>> {
    let pb = app.video.as_ref().filter(|v| v.fullscreen)?;
    let p = app.theme.palette;
    let picture: El<'_> = match &pb.frame {
        Some(handle) => image(handle.clone())
            .width(Length::Fill)
            .height(Length::Fill)
            .content_fit(ContentFit::Contain)
            .into(),
        None => Space::new().into(),
    };
    let top = row![
        text(pb.clip.name.clone())
            .size(14)
            .font(INTER_SEMIBOLD)
            .color(theme::video_fg())
            .width(Length::Fill),
        button(icon(Icon::X, 22.0, theme::video_fg()))
            .padding(8)
            .style(theme::video_control(p))
            .on_press(ui(Ui::VideoFullscreen)),
    ]
    .align_y(Alignment::Center)
    .padding(12);
    let body = stack![
        picture,
        button(Space::new())
            .width(Length::Fill)
            .height(Length::Fill)
            .padding(0)
            .style(theme::video_surface())
            .on_press(ui(Ui::VideoToggle)),
        column![
            container(top)
                .width(Length::Fill)
                .style(theme::bg(theme::video_scrim())),
            Space::new().height(Length::Fill),
            controls(app, pb, true),
        ],
    ];
    Some(opaque(
        container(body)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(theme::bg(theme::video_bg())),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn download_fraction_is_clamped_and_zero_when_unknown() {
        assert_eq!(download_fraction(50, Some(100)), 0.5);
        assert_eq!(download_fraction(500, Some(100)), 1.0);
        assert_eq!(download_fraction(50, None), 0.0);
        assert_eq!(download_fraction(50, Some(0)), 0.0);
    }
}
