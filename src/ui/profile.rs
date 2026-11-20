//! The profile card: the popout opened from names and avatars, and the
//! right-hand panel of a 1:1 DM. Banner or accent colour, theme-colour
//! gradient, avatar with decoration, styled name and server tag, badges,
//! activity / custom status, bio, member-since dates, roles and mutual
//! servers / friends, plus the profile-effect overlay on the popout.

use chrono::{DateTime, Local};
use iced::widget::{Space, column, container, image, row, scrollable, text};
use iced::{Alignment, Background, Border, Color, Gradient, Length, Padding, Point, Radians, Size};

use super::cosmetics::{self, Placed};
use super::theme::{self, INTER_BOLD, INTER_MEDIUM, INTER_SEMIBOLD};
use super::{El, with_presence};
use crate::app::App;
use crate::app::cosmetics::Phase;
use crate::app::profile::CARD_W;
use crate::net::cosmetics::{self as cos, Activity, ActivityKind, Profile, elapsed_label};
use crate::net::model::*;

/// Banners are 17:6 (600x212 and 340x120 on the official card).
fn banner_height(width: f32) -> f32 {
    (width * 6.0 / 17.0).round()
}
const AVATAR: f32 = 80.0;
const OVERLAP: f32 = 40.0;
const RADIUS: f32 = 8.0;
const BADGE: f32 = 22.0;

/// The popout card and where to put it, when one is open.
pub fn popout(app: &App) -> Option<(El<'_>, Point)> {
    let (po, profile) = app.open_profile()?;
    let user = known_user(app, po.user, profile);
    let card = card(app, &user, profile, po.guild, CARD_W, true);
    Some((card, po.at))
}

/// The DM panel's profile (no effect, fills the panel).
pub fn panel<'a>(app: &'a App, u: &User) -> El<'a> {
    let profile = app.profiles.get((u.id, None));
    let user = known_user(app, u.id, profile);
    let w = super::members::WIDTH;
    scrollable(card(app, &user, profile, None, w, false))
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new().width(4).scroller_width(4),
        ))
        .style(theme::thin_scrollbar(app.theme.palette, false))
        .height(Length::Fill)
        .into()
}

/// The freshest `User`: the profile's, else the one we know from lists.
fn known_user(app: &App, id: UserId, profile: Option<&Profile>) -> User {
    profile
        .map(|p| p.user.clone())
        .or_else(|| app.chat.users.get(&id).cloned())
        .unwrap_or_else(|| User {
            id,
            username: "Unknown".into(),
            ..User::default()
        })
}

fn card<'a>(
    app: &'a App,
    user: &User,
    profile: Option<&Profile>,
    guild: Option<GuildId>,
    width: f32,
    effects: bool,
) -> El<'a> {
    let p = app.theme.palette;
    let colors = profile.and_then(|pr| pr.theme_colors);
    let base_bg = colors.map_or(p.bg_elevated, |(a, _)| darken(theme::from_int(a), 0.55));
    let content = body(app, user, profile, guild, width, base_bg);
    // The overlay slot always exists (empty when there is no effect) so the
    // tree keeps its shape while effects load.
    let overlay: El<'a> = if effects {
        effect_overlay(app, width)
    } else {
        Space::new().into()
    };
    let radius = if effects { RADIUS } else { 0.0 };
    let bg = move |_: &iced::Theme| container::Style {
        background: Some(match colors {
            Some((a, b)) => Background::Gradient(Gradient::Linear(
                iced::gradient::Linear::new(Radians(std::f32::consts::PI))
                    .add_stop(0.0, darken(theme::from_int(a), 0.55))
                    .add_stop(1.0, darken(theme::from_int(b), 0.45)),
            )),
            None => Background::Color(p.bg_elevated),
        }),
        border: Border {
            radius: radius.into(),
            color: p.border,
            width: if effects { 1.0 } else { 0.0 },
        },
        ..container::Style::default()
    };
    container(iced::widget::stack![content, overlay])
        .width(width)
        .clip(true)
        .style(bg)
        .into()
}

/// Mixes `c` towards black by `amount` (0 keeps it).
fn darken(c: Color, amount: f32) -> Color {
    let k = 1.0 - amount;
    Color::from_rgb(c.r * k, c.g * k, c.b * k)
}

fn body<'a>(
    app: &'a App,
    user: &User,
    profile: Option<&Profile>,
    guild: Option<GuildId>,
    width: f32,
    ring: Color,
) -> El<'a> {
    let p = app.theme.palette;
    let colors = profile.and_then(|pr| pr.theme_colors);

    // Banner or accent colour.
    let banner_h = banner_height(width);
    let banner: El<'a> = match profile.and_then(|pr| app.banner_frame(pr)) {
        // Same aspect as the card: no cropping is needed (tiny-skia does not
        // clip a covering image to its bounds).
        Some(h) => image(h.clone())
            .width(Length::Fill)
            .height(banner_h)
            .content_fit(iced::ContentFit::Fill)
            .into(),
        None => {
            let c = profile
                .and_then(|pr| pr.accent_color)
                .or(user.accent_color)
                .map(theme::from_int)
                .or(colors.map(|(a, _)| theme::from_int(a)))
                .unwrap_or(p.accent);
            container(Space::new())
                .width(Length::Fill)
                .height(banner_h)
                .style(theme::bg(c))
                .into()
        }
    };
    let presence = app.chat.presence_of(user.id);
    let avatar = with_presence(
        app,
        cosmetics::avatar_decorated(app, user, AVATAR, Phase::Popout),
        AVATAR,
        presence,
        ring,
    );
    let head = iced::widget::stack![
        column![banner, Space::new().height(OVERLAP)].width(Length::Fill),
        container(avatar).padding(Padding::ZERO.top(banner_h - OVERLAP).left(16)),
    ];

    // Name, username, pronouns, badges.
    let display = user.display_name().to_owned();
    let mut name_row = row![cosmetics::name(
        app,
        user,
        &display,
        20.0,
        INTER_BOLD,
        p.header_text
    )]
    .spacing(8)
    .align_y(Alignment::Center);
    if let Some(tag) = cosmetics::tag_chip(app, user) {
        name_row = name_row.push(tag);
    }
    if user.bot {
        name_row = name_row.push(
            container(
                text("BOT")
                    .size(10)
                    .font(INTER_SEMIBOLD)
                    .color(p.accent_text),
            )
            .padding(Padding::from([1, 4]))
            .style(theme::rounded_bg(p.accent, 3.0)),
        );
    }
    let mut sub = user.username.clone();
    if let Some(pr) = profile.filter(|pr| !pr.pronouns.is_empty()) {
        sub = format!("{sub} \u{b7} {}", pr.pronouns);
    }
    let mut ident = column![
        name_row,
        text(sub)
            .size(14)
            .color(p.text_secondary)
            .wrapping(text::Wrapping::None)
    ]
    .spacing(2);
    if let Some(pr) = profile
        && !pr.badges.is_empty()
    {
        let mut badges = row![].spacing(6).align_y(Alignment::Center);
        for b in pr.badges.iter().take(12) {
            let icon: El<'a> = match app.badge_icon_handle(&b.icon) {
                Some(h) => image(h.clone()).width(BADGE).height(BADGE).into(),
                // Neutral placeholder: no brand asset is bundled.
                None => container(Space::new())
                    .width(BADGE)
                    .height(BADGE)
                    .style(theme::rounded_bg(p.bg_hover, BADGE / 2.0))
                    .into(),
            };
            badges = badges.push(icon);
        }
        ident = ident.push(container(badges).padding(Padding::ZERO.top(6)));
    }

    // Inner panel.
    let inner_bg = Color {
        a: 0.45,
        ..p.bg_app
    };
    let mut sections = column![].spacing(14);
    let acts = app.chat.activities_of(user.id);
    if let Some(c) = cos::custom_status(acts) {
        sections = sections.push(custom_status(app, c));
    }
    if let Some(a) = cos::headline(acts) {
        sections = sections.push(activity(app, a));
    }
    if let Some(pr) = profile {
        if !pr.bio.is_empty() {
            sections = sections.push(section(
                app,
                "ABOUT ME",
                super::markdown::render(
                    app,
                    Snowflake(user.id.0 ^ (1 << 62)),
                    &pr.bio,
                    p.text_normal,
                    14.0,
                ),
            ));
        }
        sections = sections.push(section(
            app,
            "MEMBER SINCE",
            member_since(app, user, pr, guild),
        ));
        if guild.is_some() && !pr.roles.is_empty() {
            sections = sections.push(section(app, "ROLES", roles(app, pr, guild)));
        }
        if !pr.mutual_guilds.is_empty() || pr.mutual_friends_count > 0 {
            sections = sections.push(mutual(app, pr));
        }
    } else {
        sections = sections.push(text("Loading profile\u{2026}").size(13).color(p.text_muted));
    }
    let inner = container(sections)
        .padding(12)
        .width(Length::Fill)
        .style(theme::rounded_bg(inner_bg, RADIUS));

    column![
        head,
        container(column![ident, inner].spacing(12))
            .padding(Padding::from([0, 16]).top(4).bottom(16))
            .width(width),
    ]
    .width(width)
    .into()
}

fn section<'a>(app: &App, title: &str, content: El<'a>) -> El<'a> {
    let p = app.theme.palette;
    column![
        text(title.to_owned())
            .size(12)
            .font(INTER_BOLD)
            .color(p.header_text),
        content
    ]
    .spacing(4)
    .into()
}

fn custom_status<'a>(app: &'a App, a: &Activity) -> El<'a> {
    let p = app.theme.palette;
    let mut r = row![].spacing(6).align_y(Alignment::Center);
    if let Some((name, id, _)) = &a.emoji {
        match id.and_then(|id| app.image(&crate::app::emoji_url(&app.cfg.cdn_url, id))) {
            Some(h) => r = r.push(image(h.clone()).width(18).height(18)),
            // A unicode emoji arrives as its glyph (the mock uses names).
            None if id.is_none() && !name.is_ascii() => {
                r = r.push(text(name.clone()).size(15));
            }
            None => {}
        }
    }
    if let Some(s) = &a.state {
        r = r.push(text(s.clone()).size(14).color(p.text_normal));
    }
    r.into()
}

fn activity<'a>(app: &'a App, a: &Activity) -> El<'a> {
    let p = app.theme.palette;
    let header = match a.kind {
        ActivityKind::Playing => "PLAYING A GAME".to_owned(),
        k => k.verb().to_uppercase(),
    };
    let art: El<'a> = match a
        .large_image
        .as_deref()
        .and_then(|i| app.activity_image(a, i))
    {
        Some(h) => image(h.clone()).width(56).height(56).into(),
        None => container(Space::new())
            .width(56)
            .height(56)
            .style(theme::rounded_bg(p.bg_hover, 6.0))
            .into(),
    };
    let mut info = column![
        text(a.name.clone())
            .size(14)
            .font(INTER_SEMIBOLD)
            .color(p.header_text)
            .wrapping(text::Wrapping::None)
    ]
    .spacing(1);
    for line in [&a.details, &a.state].into_iter().flatten() {
        info = info.push(
            text(line.clone())
                .size(13)
                .color(p.text_normal)
                .wrapping(text::Wrapping::None),
        );
    }
    if let Some(start) = a.start_ms {
        let now = crate::app::now_ms();
        let label = match a.end_ms {
            Some(end) => format!(
                "{} / {}",
                elapsed_label(start, now.min(end)),
                elapsed_label(start, end)
            ),
            None => format!("{} elapsed", elapsed_label(start, now)),
        };
        info = info.push(text(label).size(12).color(p.text_muted));
    }
    column![
        text(header).size(12).font(INTER_BOLD).color(p.header_text),
        row![art, info.width(Length::Fill)]
            .spacing(10)
            .align_y(Alignment::Center),
    ]
    .spacing(6)
    .into()
}

fn fmt_date(ms: i64) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|d| d.with_timezone(&Local).format("%b %-d, %Y").to_string())
        .unwrap_or_default()
}

fn member_since<'a>(app: &'a App, user: &User, pr: &Profile, guild: Option<GuildId>) -> El<'a> {
    let p = app.theme.palette;
    let created = fmt_date(user.id.timestamp_ms() as i64);
    let mut r = row![text(created).size(14).color(p.text_normal)]
        .spacing(8)
        .align_y(Alignment::Center);
    if let (Some(g), Some(joined)) = (guild.and_then(|g| app.chat.guild(g)), &pr.joined_at)
        && let Ok(d) = DateTime::parse_from_rfc3339(joined)
    {
        r = r.push(text("\u{b7}").size(14).color(p.text_muted)).push(
            text(format!(
                "{} {}",
                ellipsize_name(&g.name),
                fmt_date(d.timestamp_millis())
            ))
            .size(14)
            .color(p.text_normal)
            .wrapping(text::Wrapping::None),
        );
    }
    r.into()
}

fn ellipsize_name(s: &str) -> String {
    super::ellipsize(s, 110.0, 14.0, false)
}

fn roles<'a>(app: &'a App, pr: &Profile, guild: Option<GuildId>) -> El<'a> {
    let p = app.theme.palette;
    let Some(g) = guild.and_then(|g| app.chat.guild(g)) else {
        return Space::new().into();
    };
    let mut chips: Vec<El<'a>> = Vec::new();
    for r in g.roles.iter().filter(|r| pr.roles.contains(&r.id)) {
        let dot = if r.color != 0 {
            theme::from_int(r.color)
        } else {
            p.text_muted
        };
        chips.push(
            container(
                row![
                    container(Space::new())
                        .width(10)
                        .height(10)
                        .style(theme::rounded_bg(dot, 5.0)),
                    text(r.name.clone())
                        .size(12)
                        .font(INTER_MEDIUM)
                        .color(p.text_normal),
                ]
                .spacing(5)
                .align_y(Alignment::Center),
            )
            .padding(Padding::from([3, 8]))
            .style(theme::rounded_bg(p.bg_hover, 6.0))
            .into(),
        );
    }
    iced::widget::Row::with_children(chips)
        .spacing(4)
        .align_y(Alignment::Center)
        .wrap()
        .vertical_spacing(4)
        .into()
}

fn mutual<'a>(app: &'a App, pr: &Profile) -> El<'a> {
    let p = app.theme.palette;
    let mut col = column![].spacing(10);
    if !pr.mutual_guilds.is_empty() {
        let mut names = row![].spacing(6).align_y(Alignment::Center);
        for (id, _) in pr.mutual_guilds.iter().take(6) {
            if let Some(g) = app.chat.guild(*id) {
                names = names.push(match app.guild_icon(g) {
                    Some(h) => image(h.clone())
                        .width(24)
                        .height(24)
                        .border_radius(12.0)
                        .into(),
                    None => super::initials_circle(
                        &p,
                        &g.name,
                        theme::default_avatar_color(g.id.0),
                        24.0,
                    ),
                } as El<'a>);
            }
        }
        col = col.push(section(
            app,
            &format!("MUTUAL SERVERS \u{2014} {}", pr.mutual_guilds.len()),
            names.into(),
        ));
    }
    if pr.mutual_friends_count > 0 {
        let mut faces = row![].spacing(6).align_y(Alignment::Center);
        for f in pr.mutual_friends.iter().take(8) {
            faces = faces.push(super::avatar_circle(app, f, 24.0, Phase::Still));
        }
        col = col.push(section(
            app,
            &format!("MUTUAL FRIENDS \u{2014} {}", pr.mutual_friends_count),
            faces.into(),
        ));
    }
    col.into()
}

/// The profile-effect layers over the card, scaled so the effect canvas
/// spans the card width.
fn effect_overlay<'a>(app: &'a App, width: f32) -> El<'a> {
    let layers = app.effect_layers();
    if layers.is_empty() {
        return Space::new().into();
    }
    let (cw, _ch) = app.effect_canvas();
    let scale = width / cw;
    let mut stack = iced::widget::Stack::new()
        .width(Length::Fill)
        .height(Length::Fill);
    for l in layers {
        // The static fallback (reduced motion) has no size: fit the card.
        let (w, h, x, y) = if l.w == 0 {
            (width, width * 2.0, 0.0, 0.0)
        } else {
            (
                l.w as f32 * scale,
                l.h as f32 * scale,
                l.x as f32 * scale,
                l.y as f32 * scale,
            )
        };
        stack = stack.push(
            Placed::new(
                image(l.handle.clone()).width(w).height(h),
                Size::new(w, h),
                Point::new(x, y),
            )
            .slot(Length::Fill, Length::Fill)
            .clip(true),
        );
    }
    stack.into()
}
