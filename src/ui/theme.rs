//! Palette, typography, spacing and widget styles. The single source of
//! truth for the look: views never hard-code a colour or size.
//!
//! Values reproduce the layout and colour scheme of the official dark theme
//! (measured from public screenshots); see `docs/protocol.md` §UI reference.

use iced::widget::{button, container, scrollable, text_input};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector, font};

use crate::config::ThemeChoice;

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn rgba(hex: u32, a: f32) -> Color {
    Color::from_rgba8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8, a)
}

/// Converts a Discord integer colour (role, embed) to an iced colour.
pub fn from_int(c: u32) -> Color {
    rgb(c & 0xff_ffff)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg_app: Color,
    pub bg_sidebar: Color,
    pub bg_panel: Color,
    pub bg_elevated: Color,
    pub bg_hover: Color,
    pub bg_selected: Color,
    pub text_primary: Color,
    pub text_secondary: Color,
    pub text_muted: Color,
    pub accent: Color,
    pub accent_text: Color,
    pub danger: Color,
    pub success: Color,
    pub border: Color,
    /// Chat apps with bubbles only; Discord has none, so these are the
    /// message hover and mention highlight backgrounds.
    pub bubble_out: Color,
    pub bubble_in: Color,
    pub badge: Color,
    pub badge_muted: Color,
    // Discord-specific extras.
    pub text_normal: Color,
    pub header_text: Color,
    pub link: Color,
    pub mention_bg: Color,
    pub mention_text: Color,
    pub mention_bar: Color,
    pub composer_bg: Color,
    pub code_bg: Color,
    pub user_panel: Color,
    pub embed_bg: Color,
    pub spoiler: Color,
    pub channel_icon: Color,
    pub rail_pill: Color,
    /// Presence dots (online is `success`, do-not-disturb is `danger`).
    pub status_idle: Color,
    pub status_offline: Color,
}

impl Palette {
    pub const DARK: Palette = Palette {
        bg_app: rgb(0x1e1f22),
        bg_sidebar: rgb(0x2b2d31),
        bg_panel: rgb(0x313338),
        bg_elevated: rgb(0x111214),
        bg_hover: rgba(0x4e5058, 0.3),
        bg_selected: rgba(0x4e5058, 0.6),
        text_primary: rgb(0xf2f3f5),
        text_secondary: rgb(0xb5bac1),
        text_muted: rgb(0x949ba4),
        accent: rgb(0x5865f2),
        accent_text: rgb(0xffffff),
        danger: rgb(0xf23f43),
        success: rgb(0x23a559),
        border: rgb(0x1f2023),
        bubble_out: rgb(0x2e3035),
        bubble_in: rgba(0xf0b132, 0.08),
        badge: rgb(0xf23f43),
        badge_muted: rgb(0x80848e),
        text_normal: rgb(0xdbdee1),
        header_text: rgb(0xf2f3f5),
        link: rgb(0x00a8fc),
        mention_bg: rgba(0x5865f2, 0.3),
        mention_text: rgb(0xc9cdfb),
        mention_bar: rgb(0xf0b132),
        composer_bg: rgb(0x383a40),
        code_bg: rgb(0x2b2d31),
        user_panel: rgb(0x232428),
        embed_bg: rgb(0x2b2d31),
        spoiler: rgb(0x1e1f22),
        channel_icon: rgb(0x80848e),
        rail_pill: rgb(0xf2f3f5),
        status_idle: rgb(0xf0b232),
        status_offline: rgb(0x80848e),
    };

    pub const LIGHT: Palette = Palette {
        bg_app: rgb(0xe3e5e8),
        bg_sidebar: rgb(0xf2f3f5),
        bg_panel: rgb(0xffffff),
        bg_elevated: rgb(0xffffff),
        bg_hover: rgba(0x747f8d, 0.16),
        bg_selected: rgba(0x747f8d, 0.3),
        text_primary: rgb(0x060607),
        text_secondary: rgb(0x4e5058),
        text_muted: rgb(0x5c5e66),
        accent: rgb(0x5865f2),
        accent_text: rgb(0xffffff),
        danger: rgb(0xda373c),
        success: rgb(0x23a559),
        border: rgb(0xe1e2e4),
        bubble_out: rgb(0xf2f3f5),
        bubble_in: rgba(0xf0b132, 0.1),
        badge: rgb(0xda373c),
        badge_muted: rgb(0x80848e),
        text_normal: rgb(0x313338),
        header_text: rgb(0x060607),
        link: rgb(0x006ce7),
        mention_bg: rgba(0x5865f2, 0.15),
        mention_text: rgb(0x505cdc),
        mention_bar: rgb(0xc38d0c),
        composer_bg: rgb(0xebedef),
        code_bg: rgb(0xf2f3f5),
        user_panel: rgb(0xebedef),
        embed_bg: rgb(0xf2f3f5),
        spoiler: rgb(0xb9bbbe),
        channel_icon: rgb(0x6d6f78),
        rail_pill: rgb(0x060607),
        status_idle: rgb(0xf0b232),
        status_offline: rgb(0x80848e),
    };
}

/// Spacing scale, radii, widths and font sizes (logical pixels).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub rail_width: f32,
    pub rail_icon: f32,
    pub sidebar_width: f32,
    pub header_height: f32,
    pub user_panel_height: f32,
    pub avatar: f32,
    pub avatar_small: f32,
    pub radius_small: f32,
    pub radius: f32,
    pub radius_large: f32,
    pub space_xs: f32,
    pub space_s: f32,
    pub space_m: f32,
    pub space_l: f32,
    pub font_xs: f32,
    pub font_s: f32,
    pub font_m: f32,
    pub font_l: f32,
    pub font_xl: f32,
    pub font_xxl: f32,
    pub embed_max_width: f32,
}

impl Metrics {
    pub const DEFAULT: Metrics = Metrics {
        rail_width: 72.0,
        rail_icon: 48.0,
        sidebar_width: 240.0,
        header_height: 48.0,
        user_panel_height: 52.0,
        avatar: 40.0,
        avatar_small: 32.0,
        radius_small: 3.0,
        radius: 4.0,
        radius_large: 8.0,
        space_xs: 2.0,
        space_s: 4.0,
        space_m: 8.0,
        space_l: 16.0,
        font_xs: 12.0,
        font_s: 14.0,
        font_m: 16.0,
        font_l: 20.0,
        font_xl: 24.0,
        font_xxl: 32.0,
        embed_max_width: 520.0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FerroTheme {
    pub palette: Palette,
    pub metrics: Metrics,
    pub dark: bool,
}

impl FerroTheme {
    pub fn new(choice: ThemeChoice) -> Self {
        match choice {
            ThemeChoice::Dark => Self {
                palette: Palette::DARK,
                metrics: Metrics::DEFAULT,
                dark: true,
            },
            ThemeChoice::Light => Self {
                palette: Palette::LIGHT,
                metrics: Metrics::DEFAULT,
                dark: false,
            },
        }
    }

    /// iced base theme for widgets without a custom style.
    pub fn iced_theme(&self) -> Theme {
        let p = &self.palette;
        Theme::custom(
            if self.dark {
                "Ferrocord Dark"
            } else {
                "Ferrocord Light"
            },
            iced::theme::Palette {
                background: p.bg_panel,
                text: p.text_normal,
                primary: p.accent,
                success: p.success,
                warning: p.mention_bar,
                danger: p.danger,
            },
        )
    }
}

// -- typography ---------------------------------------------------------------

pub const INTER: Font = Font::with_name("Inter");
pub const INTER_MEDIUM: Font = Font {
    weight: font::Weight::Medium,
    ..INTER
};
pub const INTER_SEMIBOLD: Font = Font {
    weight: font::Weight::Semibold,
    ..INTER
};
pub const INTER_BOLD: Font = Font {
    weight: font::Weight::Bold,
    ..INTER
};
pub const INTER_ITALIC: Font = Font {
    style: font::Style::Italic,
    ..INTER
};
pub const MONO: Font = Font::MONOSPACE;

/// Embedded font files (SIL OFL 1.1).
pub const FONT_FILES: [&[u8]; 4] = [
    include_bytes!("../../assets/fonts/Inter-Regular.otf"),
    include_bytes!("../../assets/fonts/Inter-Medium.otf"),
    include_bytes!("../../assets/fonts/Inter-SemiBold.otf"),
    include_bytes!("../../assets/fonts/Inter-Bold.otf"),
];

// -- styles -------------------------------------------------------------------

pub fn bg(color: Color) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        ..Default::default()
    }
}

pub fn rounded_bg(color: Color, radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(color)),
        border: Border {
            radius: radius.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Header bar with the subtle bottom shadow of the official client.
pub fn header(p: Palette) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(p.bg_panel)),
        shadow: Shadow {
            color: Color {
                a: 0.35,
                ..Color::BLACK
            },
            offset: Vector::new(0.0, 1.0),
            blur_radius: 2.0,
        },
        ..Default::default()
    }
}

pub fn sidebar_header(p: Palette) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(p.bg_sidebar)),
        shadow: Shadow {
            color: Color {
                a: 0.35,
                ..Color::BLACK
            },
            offset: Vector::new(0.0, 1.0),
            blur_radius: 2.0,
        },
        ..Default::default()
    }
}

/// A transparent button that shows `hover` on hover and `selected` when
/// selected (channel rows, DM rows).
pub fn list_row(p: Palette, selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let background = match (selected, status) {
            (true, _) => Some(Background::Color(p.bg_selected)),
            (false, button::Status::Hovered | button::Status::Pressed) => {
                Some(Background::Color(p.bg_hover))
            }
            _ => None,
        };
        button::Style {
            background,
            text_color: p.text_muted,
            border: Border {
                radius: Metrics::DEFAULT.radius.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Server icon on the rail: a circle that morphs into a rounded square on
/// hover / selection, and takes the accent colour.
pub fn rail_icon(
    p: Palette,
    selected: bool,
    has_image: bool,
) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let active =
            selected || matches!(status, button::Status::Hovered | button::Status::Pressed);
        let size = Metrics::DEFAULT.rail_icon;
        let radius = if active { 16.0 } else { size / 2.0 };
        let background = if has_image {
            None
        } else if active {
            Some(Background::Color(p.accent))
        } else {
            Some(Background::Color(p.bg_panel))
        };
        button::Style {
            background,
            text_color: if active { p.accent_text } else { p.text_normal },
            border: Border {
                radius: radius.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

pub fn rail_add(p: Palette) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if hovered {
                p.success
            } else {
                p.bg_panel
            })),
            text_color: if hovered { p.accent_text } else { p.success },
            border: Border {
                radius: (if hovered { 16.0 } else { 24.0 }).into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

pub fn primary_button(p: Palette) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let base = p.accent;
        let bg = match status {
            button::Status::Hovered => Color {
                r: base.r * 0.85,
                g: base.g * 0.85,
                b: base.b * 0.85,
                a: 1.0,
            },
            button::Status::Pressed => Color {
                r: base.r * 0.75,
                g: base.g * 0.75,
                b: base.b * 0.75,
                a: 1.0,
            },
            button::Status::Disabled => Color { a: 0.5, ..base },
            button::Status::Active => base,
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: p.accent_text,
            border: Border {
                radius: Metrics::DEFAULT.radius.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Borderless icon button (header, user panel).
pub fn icon_button(p: Palette) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| button::Style {
        background: matches!(status, button::Status::Hovered | button::Status::Pressed)
            .then_some(Background::Color(p.bg_hover)),
        text_color: p.text_secondary,
        border: Border {
            radius: Metrics::DEFAULT.radius.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Colour of a presence dot.
pub fn presence_color(p: &Palette, status: crate::net::model::Presence) -> Color {
    use crate::net::model::Presence::*;
    match status {
        Online => p.success,
        Idle => p.status_idle,
        Dnd => p.danger,
        Offline => p.status_offline,
    }
}

/// Floating menu / popover (context menu, emoji picker, status menu).
pub fn popup(p: Palette) -> impl Fn(&Theme) -> container::Style {
    move |_| container::Style {
        background: Some(Background::Color(p.bg_elevated)),
        border: Border {
            radius: 8.0.into(),
            width: 1.0,
            color: p.border,
        },
        shadow: Shadow {
            color: Color {
                a: 0.45,
                ..Color::BLACK
            },
            offset: Vector::new(0.0, 4.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    }
}

/// Row in a popup menu: accent fill on hover; `danger` rows turn red.
pub fn menu_item(p: Palette, danger: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (bg, fg) = match (hovered, danger) {
            (true, true) => (Some(Background::Color(p.danger)), p.accent_text),
            (true, false) => (Some(Background::Color(p.accent)), p.accent_text),
            (false, true) => (None, p.danger),
            (false, false) => (None, p.text_normal),
        };
        button::Style {
            background: bg,
            text_color: fg,
            border: Border {
                radius: Metrics::DEFAULT.radius.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

/// Reaction pill: tinted with the accent when the user reacted.
pub fn reaction_pill(p: Palette, me: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        let (bg, border) = if me {
            (
                Color {
                    a: 0.15,
                    ..p.accent
                },
                Color { a: 1.0, ..p.accent },
            )
        } else if hovered {
            (p.bg_selected, p.text_muted)
        } else {
            (p.bg_sidebar, Color::TRANSPARENT)
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: p.text_normal,
            border: Border {
                radius: 8.0.into(),
                width: 1.0,
                color: border,
            },
            ..Default::default()
        }
    }
}

/// Text-like button (links, spoiler reveal).
pub fn link_button(_p: Palette) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_, _| button::Style {
        background: None,
        ..Default::default()
    }
}

pub fn input(p: Palette, bg: Color) -> impl Fn(&Theme, text_input::Status) -> text_input::Style {
    move |_, _| text_input::Style {
        background: Background::Color(bg),
        border: Border {
            radius: Metrics::DEFAULT.radius.into(),
            width: 0.0,
            color: Color::TRANSPARENT,
        },
        icon: p.text_muted,
        placeholder: p.text_muted,
        value: p.text_normal,
        selection: Color { a: 0.4, ..p.accent },
    }
}

pub fn thin_scrollbar(
    p: Palette,
    track: bool,
) -> impl Fn(&Theme, scrollable::Status) -> scrollable::Style {
    move |_, _| {
        let rail = scrollable::Rail {
            background: track.then_some(Background::Color(p.bg_sidebar)),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
            scroller: scrollable::Scroller {
                background: Background::Color(p.bg_app),
                border: Border {
                    radius: 4.0.into(),
                    ..Default::default()
                },
            },
        };
        scrollable::Style {
            container: container::Style::default(),
            vertical_rail: rail,
            horizontal_rail: rail,
            gap: None,
            auto_scroll: scrollable::AutoScroll {
                background: Background::Color(p.bg_elevated),
                border: Border::default(),
                shadow: Shadow::default(),
                icon: p.text_normal,
            },
        }
    }
}

/// Avatar background colours for users without an avatar image (original
/// palette, chosen by user id).
pub fn default_avatar_color(id: u64) -> Color {
    const COLORS: [u32; 6] = [0x5865f2, 0x757e8a, 0x3ba55c, 0xfaa61a, 0xed4245, 0xeb459f];
    rgb(COLORS[((id >> 22) % 6) as usize].max(COLORS[(id % 6) as usize]) & 0xffffff)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presence_colours_are_distinct() {
        use crate::net::model::Presence::*;
        let p = Palette::DARK;
        let cs = [Online, Idle, Dnd, Offline].map(|s| presence_color(&p, s));
        for i in 0..cs.len() {
            for j in i + 1..cs.len() {
                assert_ne!(cs[i], cs[j]);
            }
        }
    }

    #[test]
    fn int_colours() {
        assert_eq!(from_int(0xff0000), Color::from_rgb8(255, 0, 0));
    }

    #[test]
    fn themes_differ() {
        let d = FerroTheme::new(ThemeChoice::Dark);
        let l = FerroTheme::new(ThemeChoice::Light);
        assert!(d.dark && !l.dark);
        assert_ne!(d.palette.bg_panel, l.palette.bg_panel);
    }
}
