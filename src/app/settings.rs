//! User Settings: the categories of the official settings screen, the
//! account-wide settings synced through the preloaded user settings proto
//! (`net::user_settings`), the local-only ones (`pref.settings` in the
//! SQLite meta table), the profile form and the microphone test.
//!
//! Synced settings are optimistic: a change applies at once, is queued,
//! and the queue is written after [`DEBOUNCE`] of quiet (one
//! read-modify-write `PATCH` per batch, like the official client batches
//! its proto writes). A failed save rolls back what the batch changed,
//! unless a newer change of the same setting is already queued, and shows
//! a toast.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use iced::Task;
use iced::widget::text_editor;
use serde::{Deserialize, Serialize};

use super::{App, Done, Message, Ui};
use crate::net::model::Presence;
use crate::net::rest::ProfileEdit;
use crate::net::user_settings::{self as us, CustomStatus, Edit, Synced, Theme, Value};

/// Stable ids of the settings scrollables (their scroll state survives
/// redraws; a new page scrolls back to the top).
pub fn sidebar_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("settings-sidebar")
}

pub fn content_id() -> iced::advanced::widget::Id {
    iced::advanced::widget::Id::new("settings-content")
}

/// Quiet time before queued synced changes are written.
pub const DEBOUNCE: Duration = Duration::from_millis(1500);
/// SQLite `meta` key of the local-only settings.
pub const LOCAL_KEY: &str = "pref.settings";

/// The settings screen's categories, in sidebar order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    MyAccount,
    Profiles,
    Privacy,
    Status,
    Appearance,
    Accessibility,
    VoiceVideo,
    Chat,
    Notifications,
    Keybinds,
    Language,
    Advanced,
}

impl Category {
    pub const ALL: [Category; 12] = [
        Self::MyAccount,
        Self::Profiles,
        Self::Privacy,
        Self::Status,
        Self::Appearance,
        Self::Accessibility,
        Self::VoiceVideo,
        Self::Chat,
        Self::Notifications,
        Self::Keybinds,
        Self::Language,
        Self::Advanced,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::MyAccount => "My Account",
            Self::Profiles => "Profiles",
            Self::Privacy => "Privacy & Safety",
            Self::Status => "Activity & Status",
            Self::Appearance => "Appearance",
            Self::Accessibility => "Accessibility",
            Self::VoiceVideo => "Voice & Video",
            Self::Chat => "Chat",
            Self::Notifications => "Notifications",
            Self::Keybinds => "Keybinds",
            Self::Language => "Language",
            Self::Advanced => "Advanced",
        }
    }

    /// Sidebar heading the category sits under (`None`: same as above).
    pub fn heading(self) -> Option<&'static str> {
        match self {
            Self::MyAccount => Some("USER SETTINGS"),
            Self::Status => Some("ACTIVITY SETTINGS"),
            Self::Appearance => Some("APP SETTINGS"),
            _ => None,
        }
    }

    /// Name used by `--screenshot settings-<slug>`.
    pub fn slug(self) -> &'static str {
        match self {
            Self::MyAccount => "account",
            Self::Profiles => "profiles",
            Self::Privacy => "privacy",
            Self::Status => "status",
            Self::Appearance => "appearance",
            Self::Accessibility => "accessibility",
            Self::VoiceVideo => "voice",
            Self::Chat => "chat",
            Self::Notifications => "notifications",
            Self::Keybinds => "keybinds",
            Self::Language => "language",
            Self::Advanced => "advanced",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.slug() == s)
    }
}

/// Local-only settings (the official client keeps these per device too).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalSettings {
    /// "Chat font scaling" in px (12..=24).
    pub chat_font: u8,
    /// "Zoom level" in percent (50..=200).
    pub zoom: u16,
    /// "Saturation" in percent (0..=100).
    pub saturation: u8,
    /// Desktop notifications play the system sound.
    pub notify_sound: bool,
    /// Unread mention badge on the tray icon.
    pub unread_badge: bool,
    /// Flash the taskbar entry on a new mention while unfocused.
    pub taskbar_flash: bool,
    /// Show message text in notifications (else only the sender).
    pub notify_preview: bool,
}

impl Default for LocalSettings {
    fn default() -> Self {
        Self {
            chat_font: 16,
            zoom: 100,
            saturation: 100,
            notify_sound: true,
            unread_badge: true,
            taskbar_flash: true,
            notify_preview: true,
        }
    }
}

pub const CHAT_FONT_SIZES: [u8; 7] = [12, 14, 15, 16, 18, 20, 24];
pub const ZOOM_LEVELS: [u16; 9] = [50, 67, 75, 80, 90, 100, 110, 125, 150];

impl LocalSettings {
    pub fn load(store: &crate::store::Store) -> Self {
        store
            .meta(LOCAL_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str::<Self>(&s).ok())
            .unwrap_or_default()
            .clamped()
    }

    /// Values from disk are untrusted: keep them in range.
    pub fn clamped(mut self) -> Self {
        self.chat_font = self.chat_font.clamp(12, 24);
        self.zoom = self.zoom.clamp(50, 200);
        self.saturation = self.saturation.min(100);
        self
    }
}

/// A change to a local setting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum LocalChange {
    ChatFont(u8),
    Zoom(u16),
    Saturation(u8),
    NotifySound(bool),
    UnreadBadge(bool),
    TaskbarFlash(bool),
    NotifyPreview(bool),
}

impl LocalSettings {
    pub fn apply(&mut self, c: LocalChange) {
        match c {
            LocalChange::ChatFont(v) => self.chat_font = v,
            LocalChange::Zoom(v) => self.zoom = v,
            LocalChange::Saturation(v) => self.saturation = v,
            LocalChange::NotifySound(v) => self.notify_sound = v,
            LocalChange::UnreadBadge(v) => self.unread_badge = v,
            LocalChange::TaskbarFlash(v) => self.taskbar_flash = v,
            LocalChange::NotifyPreview(v) => self.notify_preview = v,
        }
        *self = self.clone().clamped();
    }
}

/// What a failed save puts back.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Rollback {
    values: BTreeMap<Synced, Value>,
    custom: Option<Option<CustomStatus>>,
}

/// Queued edits and what they replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub edits: Vec<Edit>,
    pub rollback: Rollback,
}

/// The synced settings as the app currently applies them.
#[derive(Debug, Clone, Default)]
pub struct SyncedState {
    values: BTreeMap<Synced, Value>,
    pub custom: Option<CustomStatus>,
    /// Read from Discord at least once.
    pub loaded: bool,
    pending: Vec<Edit>,
    rollback: Rollback,
    generation: u64,
}

impl SyncedState {
    pub fn get(&self, s: Synced) -> Value {
        self.values
            .get(&s)
            .cloned()
            .unwrap_or_else(|| s.default_value())
    }

    pub fn bool(&self, s: Synced) -> bool {
        self.get(s).as_bool()
    }

    pub fn u32(&self, s: Synced) -> u32 {
        self.get(s).as_u32()
    }

    pub fn theme(&self) -> Theme {
        Theme::from_wire(self.u32(Synced::Theme))
    }

    /// A value known before Discord answers (a local mirror).
    pub fn seed(&mut self, s: Synced, v: Value) {
        if !self.loaded {
            self.values.insert(s, v);
        }
    }

    fn pending_has(&self, s: Synced) -> bool {
        self.pending
            .iter()
            .any(|e| matches!(e, Edit::Set(p, _) if *p == s))
    }

    fn pending_custom(&self) -> bool {
        self.pending
            .iter()
            .any(|e| matches!(e, Edit::CustomStatus(_)))
    }

    /// Takes the server's message; settings with a queued change keep the
    /// local value.
    pub fn load(&mut self, raw: &[u8]) {
        for s in Synced::ALL {
            if self.pending_has(s) {
                continue;
            }
            match us::get(raw, s) {
                Ok(Some(v)) => {
                    self.values.insert(s, v);
                }
                _ => {
                    self.values.remove(&s);
                }
            }
        }
        if !self.pending_custom() {
            self.custom = us::custom_status(raw).ok().flatten();
        }
        self.loaded = true;
    }

    /// Applies `e` now and queues it; returns the generation its debounce
    /// timer must carry.
    pub fn edit(&mut self, e: Edit) -> u64 {
        match &e {
            Edit::Set(s, v) => {
                if !self.pending_has(*s) {
                    self.rollback.values.insert(*s, self.get(*s));
                }
                self.values.insert(*s, v.clone());
                self.pending
                    .retain(|p| !matches!(p, Edit::Set(q, _) if q == s));
            }
            Edit::CustomStatus(c) => {
                if !self.pending_custom() {
                    self.rollback.custom = Some(self.custom.clone());
                }
                self.custom = c.clone().filter(|c| !c.is_empty());
                self.pending.retain(|p| !matches!(p, Edit::CustomStatus(_)));
            }
        }
        self.pending.push(e);
        self.generation += 1;
        self.generation
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// The queued batch, if `generation` is still the latest (a newer
    /// change restarts the debounce).
    pub fn take_due(&mut self, generation: u64) -> Option<Batch> {
        (generation == self.generation)
            .then(|| self.take_all())
            .flatten()
    }

    /// The queued batch, now (settings closed, app quitting).
    pub fn take_all(&mut self) -> Option<Batch> {
        if self.pending.is_empty() {
            return None;
        }
        Some(Batch {
            edits: std::mem::take(&mut self.pending),
            rollback: std::mem::take(&mut self.rollback),
        })
    }

    /// A save failed: put back what it changed, except settings that were
    /// changed again meanwhile.
    pub fn failed(&mut self, rollback: Rollback) {
        for (s, v) in rollback.values {
            if !self.pending_has(s) {
                self.values.insert(s, v);
            }
        }
        if let Some(c) = rollback.custom
            && !self.pending_custom()
        {
            self.custom = c;
        }
    }
}

/// "Clear after" choices of the custom status dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    Never,
    Minutes30,
    Hour1,
    Hours4,
    Day1,
}

impl Expiry {
    pub const ALL: [Expiry; 5] = [
        Self::Never,
        Self::Minutes30,
        Self::Hour1,
        Self::Hours4,
        Self::Day1,
    ];

    pub fn ms(self) -> u64 {
        const MIN: u64 = 60_000;
        match self {
            Self::Never => 0,
            Self::Minutes30 => 30 * MIN,
            Self::Hour1 => 60 * MIN,
            Self::Hours4 => 240 * MIN,
            Self::Day1 => 24 * 60 * MIN,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Never => "Don't clear",
            Self::Minutes30 => "30 minutes",
            Self::Hour1 => "1 hour",
            Self::Hours4 => "4 hours",
            Self::Day1 => "Today",
        }
    }
}

impl std::fmt::Display for Expiry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Text fields of the profile and custom status forms. They are filled
/// once when the page opens (never rebuilt from the account on every
/// frame, which would fight the cursor).
#[derive(Debug, Default)]
pub struct Forms {
    pub name: String,
    pub pronouns: String,
    pub bio: text_editor::Content,
    /// Values the form started from (the "unsaved changes" bar compares).
    original: (String, String, String),
    pub profile_loaded: bool,
    pub saving: bool,
    pub custom_text: String,
    pub custom_emoji: String,
    pub expiry: Option<Expiry>,
}

impl Forms {
    pub fn fill_profile(&mut self, name: &str, pronouns: &str, bio: &str) {
        self.name = name.to_owned();
        self.pronouns = pronouns.to_owned();
        self.bio = text_editor::Content::with_text(bio);
        self.original = (name.to_owned(), pronouns.to_owned(), bio.to_owned());
        self.profile_loaded = true;
    }

    fn bio_text(&self) -> String {
        // `text()` adds a trailing newline to the last line.
        let t = self.bio.text();
        t.strip_suffix('\n').map(str::to_owned).unwrap_or(t)
    }

    /// The changes the "Save Changes" button would send.
    pub fn profile_edit(&self) -> ProfileEdit {
        let (n, p, b) = &self.original;
        let bio = self.bio_text();
        ProfileEdit {
            global_name: (self.name != *n).then(|| self.name.clone()),
            pronouns: (self.pronouns != *p).then(|| self.pronouns.clone()),
            bio: (bio != *b).then_some(bio),
        }
    }

    pub fn profile_dirty(&self) -> bool {
        !self.profile_edit().is_empty()
    }

    fn reset_profile(&mut self) {
        let (n, p, b) = self.original.clone();
        self.fill_profile(&n, &p, &b);
    }

    fn fill_custom(&mut self, c: Option<&CustomStatus>) {
        self.custom_text = c.map(|c| c.text.clone()).unwrap_or_default();
        self.custom_emoji = c.map(|c| c.emoji_name.clone()).unwrap_or_default();
        self.expiry = None;
    }
}

/// Bio limit of the official profile editor.
pub const BIO_MAX: usize = 190;
pub const PRONOUNS_MAX: usize = 40;
pub const NAME_MAX: usize = 32;
pub const CUSTOM_STATUS_MAX: usize = 128;

/// Everything the settings screen owns.
#[derive(Debug, Default)]
pub struct SettingsState {
    pub category: Option<Category>,
    pub synced: SyncedState,
    pub local: LocalSettings,
    pub forms: Forms,
    /// Running microphone test (Voice & Video).
    pub mic_test: Option<crate::net::voice::mic_test::MicTest>,
    /// Serialises proto writes (each is a read-modify-write).
    lock: Arc<tokio::sync::Mutex<()>>,
}

impl SettingsState {
    pub fn new(local: LocalSettings) -> Self {
        Self {
            local,
            ..Default::default()
        }
    }

    pub fn category(&self) -> Category {
        self.category.unwrap_or(Category::MyAccount)
    }
}

/// Settings screen input.
#[derive(Debug, Clone)]
pub enum SettingsUi {
    Select(Category),
    /// Set a synced setting.
    Set(Synced, Value),
    Local(LocalChange),
    /// A slider moving: apply now, save on release.
    LocalPreview(LocalChange),
    SaveLocal,
    SetStatus(Presence),
    ProfileName(String),
    ProfilePronouns(String),
    BioAction(text_editor::Action),
    SaveProfile,
    ResetProfile,
    CustomText(String),
    CustomEmoji(String),
    CustomExpiry(Expiry),
    SaveCustomStatus,
    ClearCustomStatus,
    ToggleMicTest,
    /// Debounce timer of the synced changes.
    Flush(u64),
}

/// Task completions for settings.
#[derive(Debug, Clone)]
pub enum SettingsDone {
    Loaded(Result<Vec<u8>, String>),
    Saved {
        result: Result<Vec<u8>, String>,
        rollback: Rollback,
    },
    /// Own pronouns and bio.
    Profile(Result<(String, String), String>),
    ProfileSaved {
        edit: ProfileEdit,
        result: Result<(), String>,
        old_name: Option<String>,
    },
}

/// Discord's locales (the official Language page).
pub const LOCALES: [(&str, &str); 32] = [
    ("id", "Bahasa Indonesia"),
    ("da", "Dansk"),
    ("de", "Deutsch"),
    ("en-GB", "English, UK"),
    ("en-US", "English, US"),
    ("es-ES", "Español"),
    ("es-419", "Español, LATAM"),
    ("fr", "Français"),
    ("hr", "Hrvatski"),
    ("it", "Italiano"),
    ("lt", "Lietuviškai"),
    ("hu", "Magyar"),
    ("nl", "Nederlands"),
    ("no", "Norsk"),
    ("pl", "Polski"),
    ("pt-BR", "Português do Brasil"),
    ("ro", "Română"),
    ("fi", "Suomi"),
    ("sv-SE", "Svenska"),
    ("vi", "Tiếng Việt"),
    ("tr", "Türkçe"),
    ("cs", "Čeština"),
    ("el", "Ελληνικά"),
    ("bg", "български"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("hi", "हिन्दी"),
    ("th", "ไทย"),
    ("zh-CN", "中文"),
    ("ja", "日本語"),
    ("zh-TW", "繁體中文"),
    ("ko", "한국어"),
];

/// Text emoticons the "Automatically convert emoticons" setting turns into
/// emoji when a message is sent (the official client's list).
const EMOTICONS: [(&str, &str); 16] = [
    (":)", "🙂"),
    (":-)", "🙂"),
    ("(:", "🙂"),
    (":D", "😄"),
    (":-D", "😄"),
    (":(", "🙁"),
    (":-(", "🙁"),
    (";)", "😉"),
    (";-)", "😉"),
    (":P", "😛"),
    (":p", "😛"),
    (":O", "😮"),
    (":o", "😮"),
    ("<3", "❤️"),
    ("</3", "💔"),
    (":'(", "😢"),
];

/// Replaces emoticons that stand alone (whitespace or the text edges on
/// both sides) outside code. Mentions, links and custom emoji tokens are
/// left alone because they are never whitespace-delimited emoticons.
pub fn convert_emoticons(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_code = false;
    for (i, part) in text.split('`').enumerate() {
        if i > 0 {
            out.push('`');
            in_code = !in_code;
        }
        if in_code {
            out.push_str(part);
            continue;
        }
        let mut first = true;
        for word in part.split(' ') {
            if !first {
                out.push(' ');
            }
            first = false;
            // Keep newlines: convert each line piece separately.
            let mut lines = word.split('\n').peekable();
            while let Some(w) = lines.next() {
                match EMOTICONS.iter().find(|(e, _)| *e == w) {
                    Some((_, emoji)) => out.push_str(emoji),
                    None => out.push_str(w),
                }
                if lines.peek().is_some() {
                    out.push('\n');
                }
            }
        }
    }
    out
}

/// Spoiler rendering (`render_spoilers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Spoilers {
    OnClick,
    IfModerator,
    Always,
}

impl Spoilers {
    pub fn from_wire(s: &str) -> Self {
        match s {
            "ALWAYS" => Self::Always,
            "IF_MODERATOR" => Self::IfModerator,
            _ => Self::OnClick,
        }
    }

    pub fn wire(self) -> &'static str {
        match self {
            Self::OnClick => "ON_CLICK",
            Self::IfModerator => "IF_MODERATOR",
            Self::Always => "ALWAYS",
        }
    }
}

/// When stickers animate (`animate_stickers`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StickerAnim {
    Always,
    OnInteraction,
    Never,
}

impl StickerAnim {
    pub fn from_wire(v: u32) -> Self {
        match v {
            1 => Self::OnInteraction,
            2 => Self::Never,
            _ => Self::Always,
        }
    }

    pub fn wire(self) -> u32 {
        match self {
            Self::Always => 0,
            Self::OnInteraction => 1,
            Self::Never => 2,
        }
    }
}

/// 24-hour clock for `HourCycle` (0 auto: from the system locale).
pub fn uses_24h(hour_cycle: u32, system_locale: &str) -> bool {
    match hour_cycle {
        1 => false,
        2 => true,
        _ => {
            // Locales that write the time with AM/PM.
            const H12: [&str; 9] = [
                "en-US", "en-CA", "en-AU", "en-NZ", "en-PH", "en-IN", "es-MX", "hi", "ar",
            ];
            let l = system_locale.replace('_', "-");
            !H12.iter()
                .any(|p| l == *p || l.starts_with(&format!("{p}-")))
        }
    }
}

impl App {
    // -- effective values the views and reducers read ------------------------

    pub fn show_embeds(&self) -> bool {
        self.settings.synced.bool(Synced::RenderEmbeds)
    }
    pub fn show_reactions(&self) -> bool {
        self.settings.synced.bool(Synced::RenderReactions)
    }
    pub fn inline_attachments(&self) -> bool {
        self.settings.synced.bool(Synced::InlineAttachmentMedia)
    }
    pub fn inline_embed_media(&self) -> bool {
        self.settings.synced.bool(Synced::InlineEmbedMedia)
    }
    pub fn compact(&self) -> bool {
        self.settings.synced.bool(Synced::MessageDisplayCompact)
    }
    pub fn developer_mode(&self) -> bool {
        self.settings.synced.bool(Synced::DeveloperMode)
    }
    pub fn animate_emoji(&self) -> bool {
        self.settings.synced.bool(Synced::AnimateEmoji)
    }
    pub fn sticker_anim(&self) -> StickerAnim {
        StickerAnim::from_wire(self.settings.synced.u32(Synced::AnimateStickers))
    }
    /// Whether a sticker of message `msg` animates now ("Animate
    /// stickers": always, on interaction = while its message is hovered,
    /// never).
    pub fn sticker_animates(&self, msg: Option<crate::net::model::MessageId>) -> bool {
        match self.sticker_anim() {
            StickerAnim::Always => true,
            StickerAnim::Never => false,
            StickerAnim::OnInteraction => {
                msg.is_some() && self.anim.hover.and_then(|h| h.message) == msg
            }
        }
    }
    pub fn spoilers(&self) -> Spoilers {
        Spoilers::from_wire(self.settings.synced.get(Synced::RenderSpoilers).as_str())
    }
    /// Spoilers in `channel` show without a click.
    pub fn spoilers_revealed(&self, channel: Option<crate::net::model::ChannelId>) -> bool {
        match self.spoilers() {
            Spoilers::Always => true,
            Spoilers::OnClick => false,
            Spoilers::IfModerator => channel.is_some_and(|c| self.chat.can_manage_messages(c)),
        }
    }
    /// Message text size in px ("Chat font scaling").
    pub fn chat_font(&self) -> f32 {
        f32::from(self.settings.local.chat_font)
    }
    /// Window scale factor ("Zoom level").
    pub fn zoom(&self) -> f32 {
        f32::from(self.settings.local.zoom) / 100.0
    }

    /// Applies the settings that live outside the settings state: theme,
    /// clock, GIF autoplay.
    pub(super) fn apply_settings(&mut self) {
        let s = &self.settings;
        // `FERRO_THEME=light` until Discord's choice is known.
        let theme = if !s.synced.loaded && self.cfg.theme == crate::config::ThemeChoice::Light {
            Theme::Light
        } else {
            s.synced.theme()
        };
        self.theme = crate::ui::theme::FerroTheme::from_settings(
            theme,
            f32::from(s.local.saturation) / 100.0,
        );
        crate::ui::clock::set_24h(uses_24h(
            s.synced.u32(Synced::HourCycle),
            &crate::net::identity::locale(),
        ));
        self.prefs.autoplay_gifs = s.synced.bool(Synced::GifAutoPlay);
    }

    // -- tasks ------------------------------------------------------------------

    /// Mock mode: account settings as if read from Discord, and the page a
    /// `--screenshot settings-<page>` asks for.
    pub(super) fn mock_settings(&mut self, screen: Option<crate::config::ScreenshotScreen>) {
        use crate::config::ScreenshotScreen as S;
        let raw = us::apply(
            &[],
            &[
                Edit::Set(Synced::Theme, Value::U32(Theme::Dark.wire())),
                Edit::Set(Synced::DmSpamFilter, Value::U32(2)),
                Edit::Set(Synced::FriendSourceFlags, Value::U32(us::FRIENDS_ALL)),
                Edit::CustomStatus(Some(CustomStatus {
                    text: "Rewriting it in Rust".into(),
                    emoji_name: "🦀".into(),
                    ..Default::default()
                })),
            ],
        )
        .unwrap_or_default();
        self.settings.synced.load(&raw);
        self.gateway
            .set_custom_status(self.settings.synced.custom.clone());
        let name = self
            .chat
            .me
            .as_ref()
            .and_then(|u| u.global_name.clone())
            .unwrap_or_default();
        self.settings
            .forms
            .fill_profile(&name, "they/them", "Building Ferrocord 🦀");
        let custom = self.settings.synced.custom.clone();
        self.settings.forms.fill_custom(custom.as_ref());
        if let Some(S::SettingsPage(c)) = screen {
            self.settings.category = Some(c);
            if c == Category::VoiceVideo {
                let _ = self.update_voice(super::voice::VoiceUi::RefreshDevices);
                self.voice.control.set_input_level(-32.0);
            }
        }
        self.apply_settings();
    }

    /// Reads the synced settings (after READY and whenever the settings
    /// open, so a change made in another client shows).
    pub(super) fn load_user_settings(&self) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let rest = self.rest.clone();
        Task::perform(
            async move { rest.user_settings().await.map_err(|e| e.user_message()) },
            |r| Message::Done(Done::Settings(Box::new(SettingsDone::Loaded(r)))),
        )
    }

    fn load_own_profile(&self) -> Task<Message> {
        let Some(me) = self.chat.me_id() else {
            return Task::none();
        };
        if self.cfg.mock {
            return Task::done(Message::Done(Done::Settings(Box::new(
                SettingsDone::Profile(Ok(("they/them".into(), "Building Ferrocord 🦀".into()))),
            ))));
        }
        let rest = self.rest.clone();
        Task::perform(
            async move {
                rest.user_profile(me, None)
                    .await
                    .map(|p| (p.pronouns, p.bio))
                    .map_err(|e| e.user_message())
            },
            |r| Message::Done(Done::Settings(Box::new(SettingsDone::Profile(r)))),
        )
    }

    /// Writes a batch: read-modify-write of the proto, serialised.
    fn save_batch(&self, batch: Batch) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let rest = self.rest.clone();
        let lock = self.settings.lock.clone();
        let Batch { edits, rollback } = batch;
        Task::perform(
            async move {
                let _guard = lock.lock().await;
                rest.edit_user_settings(&edits)
                    .await
                    .map_err(|e| e.user_message())
            },
            move |result| {
                Message::Done(Done::Settings(Box::new(SettingsDone::Saved {
                    result,
                    rollback: rollback.clone(),
                })))
            },
        )
    }

    fn save_local_settings(&self) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let Ok(json) = serde_json::to_string(&self.settings.local) else {
            return Task::none();
        };
        let store = self.store.clone();
        Task::perform(
            async move { store.blocking(move |s| s.set_meta(LOCAL_KEY, &json)).await },
            |_| Message::Done(Done::Ignored),
        )
    }

    /// Queues a synced change and (re)starts the debounce.
    pub(super) fn edit_synced(&mut self, e: Edit) -> Task<Message> {
        let generation = self.settings.synced.edit(e.clone());
        self.after_synced_change(&e);
        Task::perform(async { tokio::time::sleep(DEBOUNCE).await }, move |()| {
            Message::Ui(Ui::Settings(SettingsUi::Flush(generation)))
        })
    }

    /// Side effects of a synced value changing (locally or from Discord).
    fn after_synced_change(&mut self, e: &Edit) {
        match e {
            Edit::Set(Synced::Status, v) => {
                let p = Presence::from_wire(v.as_str());
                self.chat.my_status = p;
                self.gateway.set_status(p);
            }
            Edit::CustomStatus(_) => {
                self.gateway
                    .set_custom_status(self.settings.synced.custom.clone());
            }
            _ => {}
        }
        self.apply_settings();
    }

    /// Writes queued changes now (the settings screen closed).
    pub(super) fn flush_settings(&mut self) -> Task<Message> {
        match self.settings.synced.take_all() {
            Some(b) => self.save_batch(b),
            None => Task::none(),
        }
    }

    pub(super) fn open_settings_screen(&mut self) -> Task<Message> {
        if !self.settings.forms.profile_loaded {
            let name = self
                .chat
                .me
                .as_ref()
                .and_then(|u| u.global_name.clone())
                .unwrap_or_default();
            self.settings.forms.fill_profile(&name, "", "");
            self.settings.forms.profile_loaded = false;
        }
        let custom = self.settings.synced.custom.clone();
        self.settings.forms.fill_custom(custom.as_ref());
        Task::batch([self.load_user_settings(), self.load_own_profile()])
    }

    /// Leaves the screen: pending writes go now, the mic test stops.
    pub(super) fn close_settings_screen(&mut self) -> Task<Message> {
        self.settings.mic_test = None;
        self.flush_settings()
    }

    pub(super) fn update_settings(&mut self, u: SettingsUi) -> Task<Message> {
        let forms = &mut self.settings.forms;
        match u {
            SettingsUi::Select(c) => {
                self.settings.category = Some(c);
                let top = iced::widget::operation::scroll_to(
                    content_id(),
                    iced::widget::scrollable::AbsoluteOffset {
                        x: None,
                        y: Some(0.0),
                    },
                );
                if c != Category::VoiceVideo {
                    self.settings.mic_test = None;
                    return top;
                }
                Task::batch([
                    top,
                    self.update_voice(super::voice::VoiceUi::RefreshDevices),
                ])
            }
            SettingsUi::Set(s, v) => self.edit_synced(Edit::Set(s, v)),
            SettingsUi::SetStatus(p) => {
                self.edit_synced(Edit::Set(Synced::Status, Value::Str(p.wire().into())))
            }
            SettingsUi::Local(c) => {
                self.settings.local.apply(c);
                self.apply_settings();
                self.save_local_settings()
            }
            SettingsUi::LocalPreview(c) => {
                self.settings.local.apply(c);
                self.apply_settings();
                Task::none()
            }
            SettingsUi::SaveLocal => self.save_local_settings(),
            SettingsUi::ProfileName(s) => {
                forms.name = s.chars().take(NAME_MAX).collect();
                Task::none()
            }
            SettingsUi::ProfilePronouns(s) => {
                forms.pronouns = s.chars().take(PRONOUNS_MAX).collect();
                Task::none()
            }
            SettingsUi::BioAction(a) => {
                let edit = a.is_edit();
                forms.bio.perform(a);
                if edit && forms.bio_text().chars().count() > BIO_MAX {
                    let cut: String = forms.bio_text().chars().take(BIO_MAX).collect();
                    forms.bio = text_editor::Content::with_text(&cut);
                }
                Task::none()
            }
            SettingsUi::ResetProfile => {
                forms.reset_profile();
                Task::none()
            }
            SettingsUi::SaveProfile => self.save_profile(),
            SettingsUi::CustomText(s) => {
                forms.custom_text = s.chars().take(CUSTOM_STATUS_MAX).collect();
                Task::none()
            }
            SettingsUi::CustomEmoji(s) => {
                // One emoji (a grapheme cluster), like the official picker.
                use unicode_segmentation::UnicodeSegmentation;
                forms.custom_emoji = s.trim().graphemes(true).last().unwrap_or("").to_owned();
                Task::none()
            }
            SettingsUi::CustomExpiry(e) => {
                forms.expiry = Some(e);
                Task::none()
            }
            SettingsUi::SaveCustomStatus => {
                let now = super::now_ms();
                let f = &self.settings.forms;
                let c = CustomStatus {
                    text: f.custom_text.trim().to_owned(),
                    emoji_id: 0,
                    emoji_name: f.custom_emoji.clone(),
                    expires_at_ms: match f.expiry.unwrap_or(Expiry::Never) {
                        Expiry::Never => 0,
                        e => now + e.ms(),
                    },
                    created_at_ms: now,
                };
                let c = (!c.is_empty()).then_some(c);
                self.edit_synced(Edit::CustomStatus(c))
            }
            SettingsUi::ClearCustomStatus => {
                self.settings.forms.fill_custom(None);
                self.edit_synced(Edit::CustomStatus(None))
            }
            SettingsUi::ToggleMicTest => {
                if self.settings.mic_test.take().is_some() || self.voice.conn.is_some() {
                    // In a call the meter already shows the live microphone.
                    return Task::none();
                }
                if self.cfg.mock {
                    self.voice.control.set_input_level(-38.0);
                    return Task::none();
                }
                let devices = self.voice.control.devices();
                match crate::net::voice::mic_test::MicTest::start(
                    crate::net::voice::audio::cpal_factory(),
                    devices,
                    self.voice.control.clone(),
                ) {
                    Ok(t) => self.settings.mic_test = Some(t),
                    Err(e) => self.toast = Some(format!("Microphone test failed: {e}")),
                }
                Task::none()
            }
            SettingsUi::Flush(generation) => match self.settings.synced.take_due(generation) {
                Some(b) => self.save_batch(b),
                None => Task::none(),
            },
        }
    }

    fn save_profile(&mut self) -> Task<Message> {
        let edit = self.settings.forms.profile_edit();
        if edit.is_empty() || self.settings.forms.saving {
            return Task::none();
        }
        // The display name shows everywhere at once; put back on failure.
        let old_name = self.chat.me.as_ref().and_then(|u| u.global_name.clone());
        if let Some(n) = &edit.global_name {
            let n = n.trim();
            let v = (!n.is_empty()).then(|| n.to_owned());
            if let Some(me) = self.chat.me.as_mut() {
                me.global_name = v.clone();
            }
            if let super::AuthState::LoggedIn { user } = &mut self.auth {
                user.global_name = v;
            }
        }
        self.settings.forms.saving = true;
        if self.cfg.mock {
            return Task::done(Message::Done(Done::Settings(Box::new(
                SettingsDone::ProfileSaved {
                    edit,
                    result: Ok(()),
                    old_name,
                },
            ))));
        }
        let rest = self.rest.clone();
        let e = edit.clone();
        Task::perform(
            async move { rest.edit_profile(&e).await.map_err(|e| profile_error(&e)) },
            move |result| {
                Message::Done(Done::Settings(Box::new(SettingsDone::ProfileSaved {
                    edit: edit.clone(),
                    result,
                    old_name: old_name.clone(),
                })))
            },
        )
    }

    pub(super) fn on_settings_done(&mut self, d: SettingsDone) -> Task<Message> {
        match d {
            SettingsDone::Loaded(Ok(raw)) => {
                let before_status = self.settings.synced.get(Synced::Status);
                let before_custom = self.settings.synced.custom.clone();
                self.settings.synced.load(&raw);
                // Discord's stored status / custom status is the account's
                // presence: apply it like the official client does.
                let status = self.settings.synced.get(Synced::Status);
                if status != before_status || !self.settings.synced.loaded {
                    self.after_synced_change(&Edit::Set(Synced::Status, status));
                }
                if self.settings.synced.custom != before_custom {
                    self.after_synced_change(&Edit::CustomStatus(None));
                }
                self.apply_settings();
                let gifs = self.prefs.autoplay_gifs;
                let mirror = self.save_pref(super::window::KEY_AUTOPLAY_GIFS, gifs);
                if self.settings_open {
                    let custom = self.settings.synced.custom.clone();
                    self.settings.forms.fill_custom(custom.as_ref());
                }
                mirror
            }
            SettingsDone::Loaded(Err(e)) => {
                tracing::debug!(error = %e, "user settings unavailable");
                Task::none()
            }
            SettingsDone::Saved {
                result: Ok(raw), ..
            } => {
                self.settings.synced.load(&raw);
                self.apply_settings();
                Task::none()
            }
            SettingsDone::Saved {
                result: Err(e),
                rollback,
            } => {
                let status_before = self.settings.synced.get(Synced::Status);
                let custom_before = self.settings.synced.custom.clone();
                self.settings.synced.failed(rollback);
                let status = self.settings.synced.get(Synced::Status);
                if status != status_before {
                    self.after_synced_change(&Edit::Set(Synced::Status, status));
                }
                if self.settings.synced.custom != custom_before {
                    self.after_synced_change(&Edit::CustomStatus(None));
                }
                self.apply_settings();
                self.toast = Some(format!("Could not save your settings: {e}"));
                Task::none()
            }
            SettingsDone::Profile(Ok((pronouns, bio))) => {
                let f = &mut self.settings.forms;
                if !f.profile_dirty() {
                    let name = f.name.clone();
                    f.fill_profile(&name, &pronouns, &bio);
                }
                Task::none()
            }
            SettingsDone::Profile(Err(e)) => {
                tracing::debug!(error = %e, "own profile unavailable");
                self.settings.forms.profile_loaded = true;
                Task::none()
            }
            SettingsDone::ProfileSaved {
                edit,
                result,
                old_name,
            } => {
                let f = &mut self.settings.forms;
                f.saving = false;
                match result {
                    Ok(()) => {
                        let (mut n, mut p, mut b) = f.original.clone();
                        if let Some(v) = edit.global_name {
                            n = v.trim().to_owned();
                        }
                        if let Some(v) = edit.pronouns {
                            p = v.trim().to_owned();
                        }
                        if let Some(v) = edit.bio {
                            b = v.trim_end().to_owned();
                        }
                        f.original = (n, p, b);
                        // The fetched profile popout is stale now.
                        if let Some(me) = self.chat.me_id() {
                            self.profiles.forget(me);
                        }
                    }
                    Err(e) => {
                        if edit.global_name.is_some() {
                            if let Some(me) = self.chat.me.as_mut() {
                                me.global_name = old_name.clone();
                            }
                            if let super::AuthState::LoggedIn { user } = &mut self.auth {
                                user.global_name = old_name;
                            }
                        }
                        self.toast = Some(format!("Could not save your profile: {e}"));
                    }
                }
                Task::none()
            }
        }
    }
}

/// Discord explains rejected profile values (too long, banned words) in
/// the 400 body; other errors get the generic message.
fn profile_error(e: &crate::error::Error) -> String {
    match e {
        crate::error::Error::Http {
            status: 400,
            message,
        } if !message.is_empty() => message.chars().take(200).collect(),
        e => e.user_message(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(edits: &[Edit]) -> Vec<u8> {
        us::apply(&[], edits).unwrap()
    }

    #[test]
    fn edits_apply_now_and_debounce_by_generation() {
        let mut s = SyncedState::default();
        assert!(s.bool(Synced::RenderEmbeds));
        let g1 = s.edit(Edit::Set(Synced::RenderEmbeds, Value::Bool(false)));
        assert!(!s.bool(Synced::RenderEmbeds));
        let g2 = s.edit(Edit::Set(Synced::Theme, Value::U32(2)));
        // The first timer is stale: a newer change restarted the debounce.
        assert!(s.take_due(g1).is_none());
        let b = s.take_due(g2).unwrap();
        assert_eq!(b.edits.len(), 2);
        assert!(!s.has_pending());
        assert!(s.take_due(g2).is_none());
    }

    #[test]
    fn repeated_changes_send_the_last_value_and_roll_back_to_the_first() {
        let mut s = SyncedState::default();
        s.load(&raw(&[Edit::Set(Synced::Theme, Value::U32(2))]));
        s.edit(Edit::Set(Synced::Theme, Value::U32(3)));
        s.edit(Edit::Set(Synced::Theme, Value::U32(4)));
        let b = s.take_all().unwrap();
        assert_eq!(b.edits, [Edit::Set(Synced::Theme, Value::U32(4))]);
        s.failed(b.rollback);
        assert_eq!(s.theme(), Theme::Light);
    }

    #[test]
    fn rollback_skips_settings_changed_again() {
        let mut s = SyncedState::default();
        s.edit(Edit::Set(Synced::RenderEmbeds, Value::Bool(false)));
        s.edit(Edit::Set(Synced::RenderReactions, Value::Bool(false)));
        let b = s.take_all().unwrap();
        // Changed again while the save was in flight.
        s.edit(Edit::Set(Synced::RenderReactions, Value::Bool(true)));
        s.failed(b.rollback);
        assert!(s.bool(Synced::RenderEmbeds));
        assert!(s.bool(Synced::RenderReactions));
        assert!(s.has_pending());
    }

    #[test]
    fn server_load_keeps_pending_values() {
        let mut s = SyncedState::default();
        s.edit(Edit::Set(Synced::DeveloperMode, Value::Bool(true)));
        s.load(&raw(&[
            Edit::Set(Synced::DeveloperMode, Value::Bool(false)),
            Edit::Set(Synced::Locale, Value::Str("de".into())),
        ]));
        assert!(s.bool(Synced::DeveloperMode));
        assert_eq!(s.get(Synced::Locale).as_str(), "de");
        assert!(s.loaded);
    }

    #[test]
    fn custom_status_edit_and_rollback() {
        let mut s = SyncedState::default();
        let c = CustomStatus {
            text: "afk".into(),
            ..Default::default()
        };
        s.edit(Edit::CustomStatus(Some(c.clone())));
        assert_eq!(s.custom, Some(c));
        let b = s.take_all().unwrap();
        s.failed(b.rollback);
        assert_eq!(s.custom, None);
    }

    #[test]
    fn emoticons() {
        assert_eq!(convert_emoticons("hi :) there"), "hi 🙂 there");
        assert_eq!(convert_emoticons(":D"), "😄");
        assert_eq!(convert_emoticons("love <3\nbye :("), "love ❤️\nbye 🙁");
        // Not alone, inside code, or part of a word / URL: untouched.
        assert_eq!(
            convert_emoticons("x:) `:)` https://a.b/:P"),
            "x:) `:)` https://a.b/:P"
        );
        assert_eq!(convert_emoticons("<:emoji:123> :p"), "<:emoji:123> 😛");
    }

    #[test]
    fn hour_cycle() {
        assert!(!uses_24h(0, "en-US"));
        assert!(uses_24h(0, "de-DE"));
        assert!(uses_24h(0, "en-GB"));
        assert!(uses_24h(2, "en-US"));
        assert!(!uses_24h(1, "fr"));
    }

    #[test]
    fn local_settings_are_clamped() {
        let l: LocalSettings =
            serde_json::from_str(r#"{"chat_font": 200, "zoom": 3, "saturation": 250}"#).unwrap();
        let l = l.clamped();
        assert_eq!((l.chat_font, l.zoom, l.saturation), (24, 50, 100));
        // Missing fields take the defaults.
        assert!(l.notify_sound && l.unread_badge);
        let mut l = LocalSettings::default();
        l.apply(LocalChange::Zoom(125));
        assert_eq!(l.zoom, 125);
    }

    #[test]
    fn profile_form_tracks_changes() {
        let mut f = Forms::default();
        f.fill_profile("Ferris", "", "old bio");
        assert!(!f.profile_dirty());
        f.name = "Crab".into();
        f.bio = text_editor::Content::with_text("new bio");
        let e = f.profile_edit();
        assert_eq!(e.global_name.as_deref(), Some("Crab"));
        assert_eq!(e.bio.as_deref(), Some("new bio"));
        assert_eq!(e.pronouns, None);
        f.reset_profile();
        assert!(!f.profile_dirty());
    }

    #[test]
    fn category_slugs_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::from_slug(c.slug()), Some(c));
        }
    }
}
