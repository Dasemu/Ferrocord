//! Profile cosmetics: avatar decorations, nameplates, server tags, display
//! name styles, profiles, profile effects and activities.
//!
//! The types mirror the shapes the API sends (see `docs/protocol.md` §10),
//! so they deserialize straight from user payloads. Everything here is
//! parsed leniently: a malformed cosmetic must never break a message or a
//! member list, it is simply dropped.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::model::{GuildId, RoleId, Snowflake, User, UserId};

/// Deserializes `T`, or `None` when the value has an unexpected shape.
pub fn lenient<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let v = Option::<Value>::deserialize(d)?;
    Ok(v.and_then(|v| serde_json::from_value(v).ok()))
}

/// Colour integers from the API are `0xRRGGBB`; clamp anything else.
pub fn rgb24(c: i64) -> u32 {
    (c & 0xff_ffff) as u32
}

// ---------------------------------------------------------------------------
// Avatar decoration
// ---------------------------------------------------------------------------

/// `avatar_decoration_data { asset, sku_id }`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct AvatarDecoration {
    pub asset: String,
    #[serde(default)]
    pub sku_id: Option<Snowflake>,
}

/// Official clients draw the frame at this multiple of the avatar size.
pub const DECORATION_SCALE: f32 = 1.2;

impl AvatarDecoration {
    pub fn animated(&self) -> bool {
        self.asset.starts_with("a_")
    }

    /// `passthrough=true` serves the animated PNG, `false` a still frame.
    pub fn url(&self, cdn: &str, size: u32, animated: bool) -> String {
        format!(
            "{cdn}/avatar-decoration-presets/{}.png?size={size}&passthrough={animated}",
            self.asset
        )
    }
}

// ---------------------------------------------------------------------------
// Nameplate (collectibles)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Nameplate {
    /// Asset path such as `nameplates/nameplates/twilight/`.
    pub asset: String,
    #[serde(default)]
    pub palette: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub sku_id: Option<Snowflake>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Collectibles {
    #[serde(default, deserialize_with = "lenient")]
    pub nameplate: Option<Nameplate>,
}

impl Nameplate {
    /// The documented asset names are `img.png`, `static.png` and
    /// `asset.webm`; only the still image is drawn (no video decoder).
    pub fn static_url(&self, cdn: &str) -> String {
        let sep = if self.asset.ends_with('/') { "" } else { "/" };
        format!("{cdn}/assets/collectibles/{}{sep}static.png", self.asset)
    }

    /// Name tint used when the image is not loaded yet.
    pub fn palette_color(&self) -> u32 {
        match self.palette.as_str() {
            "crimson" => 0x9b2335,
            "berry" => 0x8e2c6f,
            "sky" => 0x3b8fd4,
            "teal" => 0x1f8a8a,
            "forest" => 0x2f6b3f,
            "bubble_gum" => 0xd75fa0,
            "violet" => 0x6b46c1,
            "cobalt" => 0x2a4fb5,
            "clover" => 0x4c9a2a,
            "lemon" => 0xc9b21e,
            "white" => 0xb8bcc6,
            _ => 0x4e5058,
        }
    }
}

// ---------------------------------------------------------------------------
// Server tag (primary guild)
// ---------------------------------------------------------------------------

/// `primary_guild { identity_guild_id, identity_enabled, tag, badge }`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PrimaryGuild {
    #[serde(default)]
    pub identity_guild_id: Option<Snowflake>,
    #[serde(default)]
    pub identity_enabled: Option<bool>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub badge: Option<String>,
}

impl PrimaryGuild {
    /// The tag to show, if the user displays it.
    pub fn visible_tag(&self) -> Option<&str> {
        if self.identity_enabled == Some(false) {
            return None;
        }
        self.tag.as_deref().filter(|t| !t.is_empty())
    }

    pub fn badge_url(&self, cdn: &str, size: u32) -> Option<String> {
        let (g, b) = (self.identity_guild_id?, self.badge.as_deref()?);
        if b.is_empty() {
            return None;
        }
        Some(format!("{cdn}/guild-tag-badges/{g}/{b}.png?size={size}"))
    }
}

// ---------------------------------------------------------------------------
// Display name style
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NameStyle {
    #[serde(default)]
    pub font_id: u32,
    #[serde(default)]
    pub effect_id: u32,
    #[serde(default)]
    pub colors: Vec<i64>,
}

/// How a name is painted after approximating the effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NamePaint {
    Solid(u32),
    /// Two or more stops, interpolated per character.
    Gradient(Vec<u32>),
}

impl NameStyle {
    /// Effects: 1 solid, 2 gradient, 3 neon, 4 toon, 5 pop, 6 glow (falls
    /// back to gradient), 7 prism, 8 gummy. Glow / outline / animation have
    /// no counterpart in iced text, so neon, toon and pop are drawn solid
    /// and prism / gummy as a static gradient.
    pub fn paint(&self) -> Option<NamePaint> {
        let cols: Vec<u32> = self.colors.iter().copied().map(rgb24).collect();
        let first = *cols.first()?;
        Some(match self.effect_id {
            2 | 6 | 7 | 8 if cols.len() >= 2 => NamePaint::Gradient(cols),
            _ => NamePaint::Solid(first),
        })
    }

    /// Fonts other than the default gg Sans (id 11) are not bundled; they
    /// are approximated with a bold weight of the interface font.
    pub fn bold(&self) -> bool {
        self.font_id != 0 && self.font_id != 11
    }
}

/// Linear interpolation between gradient stops: colour of character `i` of
/// `n` (stops are spread evenly).
pub fn gradient_at(stops: &[u32], i: usize, n: usize) -> u32 {
    match stops {
        [] => 0xffffff,
        [one] => *one,
        _ => {
            let t = if n <= 1 {
                0.0
            } else {
                i as f32 / (n - 1) as f32
            };
            let pos = t * (stops.len() - 1) as f32;
            let lo = (pos.floor() as usize).min(stops.len() - 2);
            let f = pos - lo as f32;
            let (a, b) = (stops[lo], stops[lo + 1]);
            let ch = |s: u32| ((a >> s) & 0xff) as f32 * (1.0 - f) + ((b >> s) & 0xff) as f32 * f;
            ((ch(16).round() as u32) << 16) | ((ch(8).round() as u32) << 8) | ch(0).round() as u32
        }
    }
}

// ---------------------------------------------------------------------------
// CDN builders
// ---------------------------------------------------------------------------

/// Static avatar frame (also the list / message rendering).
pub fn avatar_url(cdn: &str, user: &User, size: u32) -> Option<String> {
    user.avatar
        .as_ref()
        .map(|h| format!("{cdn}/avatars/{}/{h}.png?size={size}", user.id))
}

/// Animated GIF of an `a_` avatar.
pub fn avatar_anim_url(cdn: &str, user: &User, size: u32) -> Option<String> {
    let h = user.avatar.as_ref().filter(|h| h.starts_with("a_"))?;
    Some(format!("{cdn}/avatars/{}/{h}.gif?size={size}", user.id))
}

/// Banner: `animated` picks the GIF of an `a_` hash.
pub fn banner_url(
    cdn: &str,
    user: UserId,
    guild: Option<GuildId>,
    hash: &str,
    animated: bool,
    size: u32,
) -> String {
    let ext = if animated && hash.starts_with("a_") {
        "gif"
    } else {
        "png"
    };
    match guild {
        Some(g) => format!("{cdn}/guilds/{g}/users/{user}/banners/{hash}.{ext}?size={size}"),
        None => format!("{cdn}/banners/{user}/{hash}.{ext}?size={size}"),
    }
}

pub fn badge_icon_url(cdn: &str, icon: &str) -> String {
    format!("{cdn}/badge-icons/{icon}.png")
}

/// `GET /users/{id}/profile` with the query the official popout sends.
pub fn profile_path(user: UserId, guild: Option<GuildId>) -> String {
    let mut p = format!(
        "/users/{user}/profile?type=popout&with_mutual_guilds=true&with_mutual_friends=true&with_mutual_friends_count=true"
    );
    if let Some(g) = guild {
        p.push_str(&format!("&guild_id={g}"));
    }
    p
}

pub const PROFILE_EFFECTS_PATH: &str = "/user-profile-effects";

// ---------------------------------------------------------------------------
// Profile
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub id: String,
    pub description: String,
    pub icon: String,
}

/// A user's profile as shown in the popout, with the guild profile already
/// layered over the global one.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Profile {
    pub user: User,
    pub bio: String,
    pub pronouns: String,
    pub banner: Option<String>,
    /// The banner is the member's server banner.
    pub banner_guild: Option<GuildId>,
    pub accent_color: Option<u32>,
    pub theme_colors: Option<(u32, u32)>,
    pub effect: Option<Snowflake>,
    pub badges: Vec<Badge>,
    pub premium_since: Option<String>,
    pub nick: Option<String>,
    pub roles: Vec<RoleId>,
    pub joined_at: Option<String>,
    pub mutual_guilds: Vec<(GuildId, Option<String>)>,
    pub mutual_friends: Vec<User>,
    pub mutual_friends_count: u32,
}

#[derive(Debug, Default, Deserialize)]
struct WireMeta {
    #[serde(default)]
    bio: Option<String>,
    #[serde(default)]
    pronouns: Option<String>,
    #[serde(default)]
    banner: Option<String>,
    #[serde(default)]
    accent_color: Option<i64>,
    #[serde(default)]
    theme_colors: Option<Vec<i64>>,
    #[serde(default, deserialize_with = "lenient")]
    profile_effect: Option<WireEffectRef>,
}

#[derive(Debug, Default, Deserialize)]
struct WireEffectRef {
    id: Snowflake,
}

fn theme_pair(v: &Option<Vec<i64>>) -> Option<(u32, u32)> {
    match v.as_deref() {
        Some([a, b, ..]) => Some((rgb24(*a), rgb24(*b))),
        _ => None,
    }
}

/// Parses a `GET /users/{id}/profile` body. `None` when it has no `user`.
pub fn parse_profile(v: &Value, guild: Option<GuildId>) -> Option<Profile> {
    let user: User = serde_json::from_value(v.get("user")?.clone()).ok()?;
    let top_bio = v
        .pointer("/user/bio")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let global: WireMeta = v
        .get("user_profile")
        .and_then(|m| serde_json::from_value(m.clone()).ok())
        .unwrap_or_default();
    let member: Option<WireMeta> = v
        .get("guild_member_profile")
        .filter(|m| !m.is_null())
        .and_then(|m| serde_json::from_value(m.clone()).ok());
    let top_banner = v
        .pointer("/user/banner")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let top_accent = v.pointer("/user/accent_color").and_then(Value::as_i64);

    // Guild values win where they are set (an empty guild bio / pronouns
    // falls through to the global one, like the official client).
    let non_empty = |s: &Option<String>| s.clone().filter(|s| !s.is_empty());
    let mut p = Profile {
        bio: non_empty(&member.as_ref().and_then(|m| m.bio.clone()))
            .or_else(|| non_empty(&global.bio))
            .or(top_bio)
            .unwrap_or_default(),
        pronouns: non_empty(&member.as_ref().and_then(|m| m.pronouns.clone()))
            .or_else(|| non_empty(&global.pronouns))
            .unwrap_or_default(),
        accent_color: global
            .accent_color
            .or(top_accent)
            .or(user.accent_color.map(i64::from))
            .map(rgb24),
        theme_colors: member
            .as_ref()
            .and_then(|m| theme_pair(&m.theme_colors))
            .or_else(|| theme_pair(&global.theme_colors)),
        effect: member
            .as_ref()
            .and_then(|m| m.profile_effect.as_ref())
            .or(global.profile_effect.as_ref())
            .map(|e| e.id),
        ..Profile::default()
    };
    if let Some(b) = member
        .as_ref()
        .and_then(|m| m.banner.clone())
        .filter(|b| !b.is_empty())
    {
        p.banner = Some(b);
        p.banner_guild = guild;
    } else {
        p.banner = global.banner.filter(|b| !b.is_empty()).or(top_banner);
    }
    p.badges = v
        .get("badges")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|b| {
                    Some(Badge {
                        id: b.get("id")?.as_str()?.to_owned(),
                        description: b
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        icon: b.get("icon")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    p.premium_since = v
        .get("premium_since")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if let Some(m) = v.get("guild_member").filter(|m| !m.is_null()) {
        p.nick = m
            .get("nick")
            .and_then(Value::as_str)
            .filter(|n| !n.is_empty())
            .map(str::to_owned);
        p.joined_at = m
            .get("joined_at")
            .and_then(Value::as_str)
            .map(str::to_owned);
        p.roles = m
            .get("roles")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|r| r.as_str()?.parse().ok().map(Snowflake))
                    .collect()
            })
            .unwrap_or_default();
    }
    p.mutual_guilds = v
        .get("mutual_guilds")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|g| {
                    let id = g.get("id")?.as_str()?.parse().ok().map(Snowflake)?;
                    let nick = g.get("nick").and_then(Value::as_str).map(str::to_owned);
                    Some((id, nick))
                })
                .collect()
        })
        .unwrap_or_default();
    p.mutual_friends = v
        .get("mutual_friends")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|u| serde_json::from_value(u.clone()).ok())
                .collect()
        })
        .unwrap_or_default();
    p.mutual_friends_count = v
        .get("mutual_friends_count")
        .and_then(Value::as_u64)
        .map(|n| n.min(u32::MAX as u64) as u32)
        .unwrap_or(p.mutual_friends.len() as u32);
    p.user = user;
    Some(p)
}

// ---------------------------------------------------------------------------
// Profile effects
// ---------------------------------------------------------------------------

/// One animated layer of a profile effect.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EffectLayer {
    pub src: String,
    pub randomized: Vec<String>,
    pub looped: bool,
    pub width: u32,
    pub height: u32,
    pub duration_ms: u32,
    pub start_ms: u32,
    pub loop_delay_ms: u32,
    pub x: i32,
    pub y: i32,
    pub z: i32,
}

impl EffectLayer {
    /// Source for `user`: one of the randomized sources when there are any.
    pub fn source_for(&self, user: UserId) -> &str {
        if self.randomized.is_empty() {
            &self.src
        } else {
            &self.randomized[(user.0 as usize) % self.randomized.len()]
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProfileEffect {
    pub id: Snowflake,
    pub title: String,
    pub layers: Vec<EffectLayer>,
    pub static_frame: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct WireLayer {
    #[serde(default)]
    src: String,
    #[serde(default, rename = "loop")]
    looped: bool,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    duration: u32,
    #[serde(default)]
    start: u32,
    #[serde(default, alias = "loop_delay", rename = "loopDelay")]
    loop_delay: u32,
    #[serde(default)]
    position: Option<WirePos>,
    #[serde(default, alias = "z_index", rename = "zIndex")]
    z_index: i32,
    #[serde(default, alias = "randomized_sources", rename = "randomizedSources")]
    randomized: Vec<WireSrc>,
}

#[derive(Debug, Default, Deserialize)]
struct WirePos {
    #[serde(default)]
    x: i32,
    #[serde(default)]
    y: i32,
}

#[derive(Debug, Default, Deserialize)]
struct WireSrc {
    #[serde(default)]
    src: String,
}

/// Parses `GET /user-profile-effects`: `{ "profile_effect_configs": [..] }`
/// (a bare array is accepted too). Layers are sorted by z-index.
pub fn parse_effects(v: &Value) -> Vec<ProfileEffect> {
    let list = v
        .get("profile_effect_configs")
        .or_else(|| v.get("configs"))
        .unwrap_or(v);
    let Some(arr) = list.as_array() else {
        return vec![];
    };
    arr.iter()
        .filter_map(|e| {
            let id: Snowflake = e.get("id")?.as_str()?.parse().ok()?;
            let mut layers: Vec<EffectLayer> = e
                .get("effects")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|l| serde_json::from_value::<WireLayer>(l.clone()).ok())
                        .map(|l| EffectLayer {
                            src: l.src,
                            randomized: l.randomized.into_iter().map(|s| s.src).collect(),
                            looped: l.looped,
                            width: l.width,
                            height: l.height,
                            duration_ms: l.duration,
                            start_ms: l.start,
                            loop_delay_ms: l.loop_delay,
                            x: l.position.as_ref().map_or(0, |p| p.x),
                            y: l.position.as_ref().map_or(0, |p| p.y),
                            z: l.z_index,
                        })
                        .collect()
                })
                .unwrap_or_default();
            layers.sort_by_key(|l| l.z);
            Some(ProfileEffect {
                id,
                title: e
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                layers,
                static_frame: e
                    .get("staticFrameSrc")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        })
        .collect()
}

/// Which part of a layer's timeline is showing `t_ms` after the popout
/// opened: `Some(ms into the animation)` or `None` (not started / over).
pub fn layer_time(l: &EffectLayer, t_ms: u64) -> Option<u64> {
    let start = u64::from(l.start_ms);
    if t_ms < start {
        return None;
    }
    let t = t_ms - start;
    let dur = u64::from(l.duration_ms.max(1));
    if !l.looped {
        return (t < dur).then_some(t);
    }
    let period = dur + u64::from(l.loop_delay_ms);
    let r = t % period;
    (r < dur).then_some(r)
}

// ---------------------------------------------------------------------------
// Activities (presence)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    Playing,
    Streaming,
    Listening,
    Watching,
    Custom,
    Competing,
    Other,
}

impl ActivityKind {
    pub fn from_wire(t: u64) -> Self {
        match t {
            0 => Self::Playing,
            1 => Self::Streaming,
            2 => Self::Listening,
            3 => Self::Watching,
            4 => Self::Custom,
            5 => Self::Competing,
            _ => Self::Other,
        }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Self::Playing => "Playing",
            Self::Streaming => "Streaming",
            Self::Listening => "Listening to",
            Self::Watching => "Watching",
            Self::Competing => "Competing in",
            Self::Custom | Self::Other => "",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct Activity {
    pub kind: ActivityKind,
    pub name: String,
    pub details: Option<String>,
    pub state: Option<String>,
    /// Custom status emoji: `(name, id, animated)`.
    pub emoji: Option<(String, Option<Snowflake>, bool)>,
    pub application_id: Option<Snowflake>,
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    pub large_image: Option<String>,
    pub small_image: Option<String>,
}

impl std::fmt::Debug for Activity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Activity names can reveal what someone is doing: keep logs clean.
        write!(f, "Activity({:?})", self.kind)
    }
}

fn str_opt(v: &Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Timestamps are strings in the docs but numbers in practice.
fn ms_of(v: Option<&Value>) -> Option<u64> {
    match v? {
        Value::Number(n) => n.as_u64(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

/// Parses the `activities` array of a presence. At most 8 are kept.
pub fn parse_activities(v: &Value) -> Vec<Activity> {
    let Some(arr) = v.as_array() else {
        return vec![];
    };
    arr.iter()
        .take(8)
        .filter_map(|a| {
            let kind = ActivityKind::from_wire(a.get("type")?.as_u64()?);
            let assets = a.get("assets");
            let emoji = a.get("emoji").filter(|e| e.is_object()).and_then(|e| {
                Some((
                    e.get("name")?.as_str()?.to_owned(),
                    e.get("id")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse().ok())
                        .map(Snowflake),
                    e.get("animated").and_then(Value::as_bool).unwrap_or(false),
                ))
            });
            Some(Activity {
                kind,
                name: a.get("name").and_then(Value::as_str)?.to_owned(),
                details: str_opt(a, "details"),
                state: str_opt(a, "state"),
                emoji,
                application_id: a
                    .get("application_id")
                    .and_then(Value::as_str)
                    .and_then(|s| s.parse().ok())
                    .map(Snowflake),
                start_ms: ms_of(a.pointer("/timestamps/start")),
                end_ms: ms_of(a.pointer("/timestamps/end")),
                large_image: assets.and_then(|x| str_opt(x, "large_image")),
                small_image: assets.and_then(|x| str_opt(x, "small_image")),
            })
        })
        .collect()
}

/// Image URL of an activity asset (`mp:`, `spotify:` and application
/// assets; Twitch / YouTube previews are not fetched).
pub fn activity_image_url(cdn: &str, app: Option<Snowflake>, image: &str) -> Option<String> {
    if let Some(rest) = image.strip_prefix("mp:") {
        // Only plain path characters: the id is attacker-controlled.
        if rest.is_empty() || rest.contains("..") || rest.contains('?') || rest.contains('#') {
            return None;
        }
        return Some(format!("https://media.discordapp.net/{rest}"));
    }
    if let Some(id) = image.strip_prefix("spotify:") {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        return Some(format!("https://i.scdn.co/image/{id}"));
    }
    if image.contains(':') {
        return None;
    }
    if image.is_empty() || !image.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(format!("{cdn}/app-assets/{}/{image}.png", app?))
}

/// Elapsed time of an activity as `mm:ss` / `h:mm:ss`.
pub fn elapsed_label(start_ms: u64, now_ms: u64) -> String {
    let s = now_ms.saturating_sub(start_ms) / 1000;
    let (h, m, sec) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

/// The activity the popout and list should lead with: playing-type
/// activities first, custom status shown separately.
pub fn headline(list: &[Activity]) -> Option<&Activity> {
    list.iter().find(|a| a.kind != ActivityKind::Custom)
}

pub fn custom_status(list: &[Activity]) -> Option<&Activity> {
    list.iter().find(|a| a.kind == ActivityKind::Custom)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CDN: &str = "https://cdn.discordapp.com";

    #[test]
    fn cdn_builders() {
        let d = AvatarDecoration {
            asset: "a_abc".into(),
            sku_id: None,
        };
        assert!(d.animated());
        assert_eq!(
            d.url(CDN, 96, true),
            "https://cdn.discordapp.com/avatar-decoration-presets/a_abc.png?size=96&passthrough=true"
        );
        assert!(d.url(CDN, 96, false).ends_with("passthrough=false"));
        let n = Nameplate {
            asset: "nameplates/nameplates/twilight/".into(),
            ..Default::default()
        };
        assert_eq!(
            n.static_url(CDN),
            "https://cdn.discordapp.com/assets/collectibles/nameplates/nameplates/twilight/static.png"
        );
        let g = PrimaryGuild {
            identity_guild_id: Some(Snowflake(5)),
            identity_enabled: Some(true),
            tag: Some("MEOW".into()),
            badge: Some("7d17".into()),
        };
        assert_eq!(
            g.badge_url(CDN, 32).as_deref(),
            Some("https://cdn.discordapp.com/guild-tag-badges/5/7d17.png?size=32")
        );
        assert_eq!(
            banner_url(CDN, Snowflake(1), None, "a_x", true, 600),
            "https://cdn.discordapp.com/banners/1/a_x.gif?size=600"
        );
        assert!(banner_url(CDN, Snowflake(1), None, "a_x", false, 600).contains(".png"));
        assert!(
            banner_url(CDN, Snowflake(1), Some(Snowflake(2)), "h", true, 600)
                .contains("/guilds/2/users/1/banners/h.png")
        );
        assert_eq!(
            badge_icon_url(CDN, "abc"),
            "https://cdn.discordapp.com/badge-icons/abc.png"
        );
        assert_eq!(
            profile_path(Snowflake(7), Some(Snowflake(9))),
            "/users/7/profile?type=popout&with_mutual_guilds=true&with_mutual_friends=true&with_mutual_friends_count=true&guild_id=9"
        );
        let u = User {
            id: Snowflake(3),
            avatar: Some("a_hash".into()),
            ..Default::default()
        };
        assert!(avatar_anim_url(CDN, &u, 80).unwrap().contains("a_hash.gif"));
        let still = User {
            avatar: Some("hash".into()),
            ..u
        };
        assert!(avatar_anim_url(CDN, &still, 80).is_none());
    }

    #[test]
    fn hidden_tags_and_empty_badges() {
        let g = PrimaryGuild {
            identity_enabled: Some(false),
            tag: Some("X".into()),
            ..Default::default()
        };
        assert!(g.visible_tag().is_none());
        let g = PrimaryGuild {
            tag: Some(String::new()),
            ..Default::default()
        };
        assert!(g.visible_tag().is_none());
        assert!(g.badge_url(CDN, 32).is_none());
    }

    #[test]
    fn user_payload_with_cosmetics_parses_and_bad_ones_are_dropped() {
        let u: User = serde_json::from_value(json!({
            "id": "80351110224678912", "username": "nelly",
            "avatar_decoration_data": {"sku_id": "1144058844004233369", "asset": "a_fed4", "expires_at": null},
            "collectibles": {"nameplate": {"asset": "nameplates/x/angel/", "palette": "bubble_gum", "label": "L", "sku_id": "1"}},
            "primary_guild": {"identity_guild_id": "80351110224678913", "identity_enabled": true, "tag": "MEOW", "badge": "7d17"},
            "display_name_styles": {"font_id": 3, "effect_id": 2, "colors": [16711680, 255]},
            "banner": "bh", "accent_color": 16711680
        }))
        .unwrap();
        assert_eq!(u.avatar_decoration.as_ref().unwrap().asset, "a_fed4");
        assert_eq!(
            u.collectibles
                .as_ref()
                .unwrap()
                .nameplate
                .as_ref()
                .unwrap()
                .palette,
            "bubble_gum"
        );
        assert_eq!(
            u.primary_guild.as_ref().unwrap().visible_tag(),
            Some("MEOW")
        );
        assert_eq!(u.name_style.as_ref().unwrap().colors.len(), 2);
        assert_eq!(u.banner.as_deref(), Some("bh"));
        assert_eq!(u.accent_color, Some(0xff0000));
        // Wrong shapes never fail the user.
        let u: User = serde_json::from_value(json!({
            "id": "1", "username": "x",
            "avatar_decoration_data": "oops",
            "collectibles": [1, 2],
            "primary_guild": 5,
            "display_name_styles": {"colors": "red"},
            "accent_color": "red"
        }))
        .unwrap();
        assert!(u.avatar_decoration.is_none() && u.collectibles.is_none());
        assert!(u.primary_guild.is_none() && u.name_style.is_none());
        assert!(u.accent_color.is_none());
        // Explicit nulls (the API sends them for "none").
        let u: User = serde_json::from_value(
            json!({"id": "1", "username": "x", "avatar_decoration_data": null, "primary_guild": null}),
        )
        .unwrap();
        assert!(u.avatar_decoration.is_none());
    }

    #[test]
    fn name_style_paint_and_gradient() {
        let s = NameStyle {
            font_id: 11,
            effect_id: 2,
            colors: vec![0xff0000, 0x0000ff],
        };
        assert_eq!(
            s.paint(),
            Some(NamePaint::Gradient(vec![0xff0000, 0x0000ff]))
        );
        assert!(!s.bold());
        let neon = NameStyle {
            font_id: 14,
            effect_id: 3,
            colors: vec![0x00ff00],
        };
        assert_eq!(neon.paint(), Some(NamePaint::Solid(0x00ff00)));
        assert!(neon.bold());
        // A gradient with one colour degrades to solid; none paints nothing.
        let one = NameStyle {
            effect_id: 2,
            colors: vec![5],
            ..Default::default()
        };
        assert_eq!(one.paint(), Some(NamePaint::Solid(5)));
        assert_eq!(NameStyle::default().paint(), None);
        assert_eq!(gradient_at(&[0xff0000, 0x0000ff], 0, 5), 0xff0000);
        assert_eq!(gradient_at(&[0xff0000, 0x0000ff], 4, 5), 0x0000ff);
        assert_eq!(gradient_at(&[0x000000, 0xffffff], 1, 3), 0x808080);
        assert_eq!(gradient_at(&[7], 3, 9), 7);
    }

    fn profile_fixture() -> Value {
        serde_json::from_str(include_str!("../../tests/fixtures/user_profile.json")).unwrap()
    }

    #[test]
    fn profile_layers_guild_over_global() {
        let g = Some(Snowflake(80351110224678913));
        let p = parse_profile(&profile_fixture(), g).unwrap();
        assert_eq!(p.user.id, Snowflake(852892297661906993));
        // Guild bio and effect win; pronouns fall through (guild is empty).
        assert_eq!(p.bio, "Professional alien");
        assert_eq!(p.pronouns, "gnarp/gnap");
        assert_eq!(p.effect, Some(Snowflake(1139323097930027068)));
        assert_eq!(p.theme_colors, Some((0x112233, 0x445566)));
        assert_eq!(
            p.banner.as_deref(),
            Some("a_17a0757cf6121ccc07546de9bff3edb2")
        );
        assert_eq!(p.banner_guild, None);
        assert_eq!(p.badges.len(), 2);
        assert_eq!(p.badges[0].icon, "icon1");
        assert_eq!(p.roles, vec![Snowflake(11), Snowflake(12)]);
        assert_eq!(p.joined_at.as_deref(), Some("2016-01-01T00:00:00.00+00:00"));
        assert_eq!(p.mutual_guilds.len(), 2);
        assert_eq!(p.mutual_guilds[0].1.as_deref(), Some("Liena"));
        assert_eq!(p.mutual_friends.len(), 1);
        assert_eq!(p.mutual_friends_count, 100);
        assert_eq!(
            p.premium_since.as_deref(),
            Some("2016-01-01T00:00:00.00+00:00")
        );
    }

    #[test]
    fn profile_without_guild_and_with_blocked_fields() {
        let mut v = profile_fixture();
        v.as_object_mut().unwrap().remove("guild_member_profile");
        v.as_object_mut().unwrap().remove("guild_member");
        let p = parse_profile(&v, None).unwrap();
        assert_eq!(p.bio, "Professional smoothbrain");
        assert!(p.roles.is_empty() && p.joined_at.is_none());
        // Blocked: user_profile / badges missing or null.
        let blocked =
            json!({"user": {"id": "5", "username": "b"}, "user_profile": null, "badges": null});
        let p = parse_profile(&blocked, None).unwrap();
        assert!(p.bio.is_empty() && p.badges.is_empty());
        assert!(parse_profile(&json!({}), None).is_none());
    }

    #[test]
    fn guild_banner_marks_the_guild() {
        let v = json!({
            "user": {"id": "5", "username": "b"},
            "user_profile": {"banner": "g"},
            "guild_member_profile": {"guild_id": "9", "banner": "m"}
        });
        let p = parse_profile(&v, Some(Snowflake(9))).unwrap();
        assert_eq!(p.banner.as_deref(), Some("m"));
        assert_eq!(p.banner_guild, Some(Snowflake(9)));
    }

    #[test]
    fn effects_parse_sorted_by_z() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/profile_effects.json"))
                .unwrap();
        let e = parse_effects(&v);
        assert_eq!(e.len(), 2);
        let first = &e[0];
        assert_eq!(first.id, Snowflake(1139323097930027068));
        assert_eq!(first.layers.len(), 2);
        assert!(first.layers[0].z <= first.layers[1].z);
        let l = &first.layers[1];
        assert!(l.looped);
        assert_eq!(
            (l.width, l.height, l.duration_ms, l.start_ms),
            (450, 880, 3000, 500)
        );
        assert_eq!(l.loop_delay_ms, 2000);
        assert_eq!((l.x, l.y), (10, 20));
        assert_eq!(e[1].layers[0].randomized.len(), 2);
        assert!(parse_effects(&json!({"nope": 1})).is_empty());
        assert!(parse_effects(&json!([{"id": "x"}])).is_empty());
    }

    #[test]
    fn layer_timeline() {
        let mut l = EffectLayer {
            duration_ms: 1000,
            start_ms: 500,
            ..Default::default()
        };
        assert_eq!(layer_time(&l, 100), None);
        assert_eq!(layer_time(&l, 500), Some(0));
        assert_eq!(layer_time(&l, 1499), Some(999));
        // One-shot layers end.
        assert_eq!(layer_time(&l, 1500), None);
        l.looped = true;
        l.loop_delay_ms = 1000;
        assert_eq!(layer_time(&l, 1500), None); // in the delay
        assert_eq!(layer_time(&l, 2500), Some(0)); // second loop
        assert_eq!(layer_time(&l, 2750), Some(250));
        // Zero duration never divides by zero.
        l.duration_ms = 0;
        let _ = layer_time(&l, 99999);
    }

    #[test]
    fn randomized_sources_are_stable_per_user() {
        let l = EffectLayer {
            src: "s".into(),
            randomized: vec!["a".into(), "b".into()],
            ..Default::default()
        };
        assert_eq!(l.source_for(Snowflake(2)), "a");
        assert_eq!(l.source_for(Snowflake(3)), "b");
        assert_eq!(l.source_for(Snowflake(3)), l.source_for(Snowflake(3)));
        let plain = EffectLayer {
            src: "s".into(),
            ..Default::default()
        };
        assert_eq!(plain.source_for(Snowflake(9)), "s");
    }

    #[test]
    fn activities_parse() {
        let v = json!([
            {"type": 4, "name": "Custom Status", "state": "busy", "emoji": {"name": "fire", "id": "5", "animated": true}},
            {"type": 0, "name": "Factorio", "application_id": "7", "details": "Base",
             "timestamps": {"start": "1700000000000"}, "assets": {"large_image": "123", "small_image": "mp:external/x/y.png"}},
            {"type": 2, "name": "Spotify", "details": "Song", "state": "Artist", "sync_id": "abc",
             "timestamps": {"start": 1700000000000u64, "end": 1700000200000u64},
             "assets": {"large_image": "spotify:ab67616d0000b273"}},
            {"nope": 1}
        ]);
        let a = parse_activities(&v);
        assert_eq!(a.len(), 3);
        assert_eq!(
            custom_status(&a).unwrap().emoji.as_ref().unwrap().1,
            Some(Snowflake(5))
        );
        let h = headline(&a).unwrap();
        assert_eq!(
            (h.kind, h.name.as_str()),
            (ActivityKind::Playing, "Factorio")
        );
        assert_eq!(h.kind.verb(), "Playing");
        assert_eq!(h.start_ms, Some(1_700_000_000_000));
        assert_eq!(a[2].end_ms, Some(1_700_000_200_000));
        assert_eq!(ActivityKind::Listening.verb(), "Listening to");
        assert!(parse_activities(&json!("x")).is_empty());
        assert!(!format!("{:?}", a[1]).contains("Factorio"));
    }

    #[test]
    fn activity_image_urls_are_restricted() {
        let app = Some(Snowflake(7));
        assert_eq!(
            activity_image_url(CDN, app, "123").as_deref(),
            Some("https://cdn.discordapp.com/app-assets/7/123.png")
        );
        assert_eq!(
            activity_image_url(CDN, app, "spotify:ab12").as_deref(),
            Some("https://i.scdn.co/image/ab12")
        );
        assert_eq!(
            activity_image_url(CDN, app, "mp:external/k/https/x.png").as_deref(),
            Some("https://media.discordapp.net/external/k/https/x.png")
        );
        assert!(activity_image_url(CDN, app, "mp:../x").is_none());
        assert!(activity_image_url(CDN, app, "spotify:a/b").is_none());
        assert!(activity_image_url(CDN, app, "twitch:foo").is_none());
        assert!(activity_image_url(CDN, None, "123").is_none());
        assert!(activity_image_url(CDN, app, "not-digits").is_none());
    }

    #[test]
    fn elapsed_formats() {
        assert_eq!(elapsed_label(0, 65_000), "01:05");
        assert_eq!(elapsed_label(0, 3_725_000), "1:02:05");
        assert_eq!(elapsed_label(10, 5), "00:00");
    }
}
