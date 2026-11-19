//! Application state and update loop (the "Elm" side). No direct I/O:
//! everything that touches the network, disk or keyring runs in a `Task`.

pub mod images;
pub mod markdown;
pub mod message;
pub mod state;

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::{Subscription, Task};

pub use message::{Done, Message, Ui};
use state::{ChatState, Ctx, Effect};

use crate::auth::secrets::{self, KeyringStore, MemoryStore, Secret, SecretStore};
use crate::auth::{AuthEvent, AuthState, QrState, normalize_token};
use crate::config::{Config, ScreenshotScreen};
use crate::error::Redacted;
use crate::mock;
use crate::net::gateway::{self, GatewayConfig};
use crate::net::model::*;
use crate::net::remote_auth::{self, RaEvent};
use crate::net::rest::RestClient;
use crate::net::types::ClientProperties;
use crate::net::{ConnectionState, Event};
use crate::store::Store;
use crate::ui::theme::FerroTheme;

/// Minimum interval between typing notifications (the server shows the
/// indicator for ~10 s).
const TYPING_INTERVAL_MS: u64 = 8_000;
/// Messages fetched per history page.
const PAGE: u8 = 50;

/// What the main area shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Direct messages (with an optional open DM).
    Home(Option<ChannelId>),
    Guild {
        guild: GuildId,
        channel: Option<ChannelId>,
    },
}

/// Which top-level screen to render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Splash,
    Login,
    Main,
}

pub struct App {
    pub cfg: Config,
    pub theme: FerroTheme,
    secrets: Arc<dyn SecretStore>,
    store: Store,
    rest: RestClient,
    pub auth: AuthState,
    token: Option<Secret>,
    pub token_input: String,
    pub qr: Option<QrState>,
    qr_generation: u64,
    gateway_generation: u64,
    pub connection: ConnectionState,
    pub chat: ChatState,
    pub view: View,
    pub composer: String,
    last_typing_sent: HashMap<ChannelId, u64>,
    pub collapsed: HashSet<ChannelId>,
    pub revealed_spoilers: HashSet<MessageId>,
    pub images: images::ImageCache,
    pub window_focused: bool,
    pub toast: Option<String>,
    boot_at: Instant,
    first_frame_logged: bool,
    fonts_ready: bool,
    window_opened: bool,
}

pub fn now_ms() -> u64 {
    mock::now_ms()
}

impl App {
    pub fn boot(cfg: Config) -> (Self, Task<Message>) {
        let boot_at = crate::config::PROCESS_START
            .get()
            .copied()
            .unwrap_or_else(Instant::now);
        let theme = FerroTheme::new(cfg.theme);
        let secrets: Arc<dyn SecretStore> = if cfg.mock {
            Arc::new(MemoryStore::new())
        } else {
            Arc::new(KeyringStore)
        };
        let store = match Store::open(cfg.database_path().as_deref()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "cache unavailable; using in-memory cache");
                match Store::open(None) {
                    Ok(s) => s,
                    Err(e) => {
                        // In-memory SQLite failing means the process is broken.
                        tracing::error!(error = %e, "in-memory cache failed");
                        std::process::exit(1);
                    }
                }
            }
        };
        let rest = RestClient::new(&cfg.api_url, &cfg.cdn_url, None).unwrap_or_else(|e| {
            tracing::error!(error = %e, "HTTP client init failed");
            std::process::exit(1);
        });

        let mut app = App {
            theme,
            secrets,
            store,
            rest,
            auth: AuthState::Restoring,
            token: None,
            token_input: String::new(),
            qr: None,
            qr_generation: 0,
            gateway_generation: 0,
            connection: ConnectionState::Disconnected,
            chat: ChatState::default(),
            view: View::Home(None),
            composer: String::new(),
            last_typing_sent: HashMap::new(),
            collapsed: HashSet::new(),
            revealed_spoilers: HashSet::new(),
            images: images::ImageCache::default(),
            window_focused: true,
            toast: None,
            boot_at,
            first_frame_logged: false,
            fonts_ready: false,
            window_opened: false,
            cfg,
        };

        let fonts = Task::batch(
            crate::ui::theme::FONT_FILES
                .iter()
                .map(|f| iced::font::load(*f).map(|_| Message::Done(Done::Ignored))),
        )
        .chain(Task::done(Message::Done(Done::FontsLoaded)));

        let restore = if app.cfg.mock {
            app.boot_mock();
            Task::none()
        } else {
            // First frame from the SQLite cache; session restore in the
            // background.
            match app.store.load_snapshot() {
                Ok(snap) => {
                    let ready = ReadyData {
                        me: snap.me.clone(),
                        guilds: snap.guilds,
                        channels: snap.channels,
                        users: snap.users,
                        read_states: snap.read_states,
                        ..Default::default()
                    };
                    app.chat.load_ready(&ready);
                    app.view = match (snap.selected_guild, snap.selected_channel) {
                        (Some(g), c) if app.chat.guild(g).is_some() => View::Guild {
                            guild: g,
                            channel: c,
                        },
                        (_, c) => View::Home(c.filter(|c| app.chat.channels.contains_key(c))),
                    };
                }
                Err(e) => tracing::warn!(error = %e, "could not read cache snapshot"),
            }
            let s = app.secrets.clone();
            Task::perform(secrets::load_token(s), |r| {
                Message::Done(Done::TokenRestored(r.map_err(|e| e.user_message())))
            })
        };
        (app, Task::batch([fonts, restore]))
    }

    fn boot_mock(&mut self) {
        let ready = mock::ready();
        self.chat.load_ready(&ready);
        for c in ready.channels.iter().filter(|c| c.kind.is_textual()) {
            let mut h = mock::history(c.id);
            for m in &mut h {
                m.channel_id = c.id;
                m.guild_id = c.guild_id;
            }
            if let Some(last) = h.last() {
                // Keep read-state math consistent with the mock history.
                if let Some(ch) = self.chat.channels.get_mut(&c.id) {
                    ch.last_message_id = Some(last.id);
                }
                if let Some(rs) = self.chat.read_states.get_mut(&c.id)
                    && rs.last_read != Some(Snowflake(1))
                {
                    rs.last_read = Some(last.id);
                }
            }
            self.chat.insert_history(c.id, h, false, true);
        }
        let (w, h, px) = mock::mock_image(400, 225);
        self.images.insert(
            "mock://image/1".into(),
            iced::widget::image::Handle::from_rgba(w, h, px),
            (w * h * 4) as usize,
        );
        self.chat.typing.insert(
            mock::CH_GENERAL,
            vec![state::Typing {
                user_id: Snowflake(2),
                until_ms: u64::MAX,
            }],
        );
        self.connection = ConnectionState::Connected;
        let screen = self.cfg.screenshot.as_ref().map(|(s, _)| *s);
        match screen {
            Some(ScreenshotScreen::Login) => {
                self.auth = AuthState::LoggedOut { error: None };
                self.qr = Some(qr_ready("https://discord.com/ra/mock-fingerprint-not-real"));
            }
            Some(ScreenshotScreen::Dms) => {
                self.auth = AuthState::LoggedIn {
                    user: mock::user(1),
                };
                self.view = View::Home(Some(mock::DM_ALICE));
                self.chat.mark_read(mock::DM_ALICE);
                self.chat.typing.insert(
                    mock::DM_ALICE,
                    vec![state::Typing {
                        user_id: Snowflake(2),
                        until_ms: u64::MAX,
                    }],
                );
            }
            Some(ScreenshotScreen::Embeds) => {
                self.auth = AuthState::LoggedIn {
                    user: mock::user(1),
                };
                self.view = View::Guild {
                    guild: mock::GUILD_RUST,
                    channel: Some(mock::CH_SHOWCASE),
                };
                self.chat.mark_read(mock::CH_SHOWCASE);
            }
            Some(ScreenshotScreen::Guild) | None => {
                self.auth = AuthState::LoggedIn {
                    user: mock::user(1),
                };
                self.view = View::Guild {
                    guild: mock::GUILD_RUST,
                    channel: Some(mock::CH_GENERAL),
                };
                self.chat.mark_read(mock::CH_GENERAL);
            }
        }
    }

    pub fn screen(&self) -> Screen {
        match &self.auth {
            AuthState::LoggedOut { .. } => Screen::Login,
            AuthState::LoggedIn { .. } => Screen::Main,
            AuthState::Restoring | AuthState::Validating => {
                if self.chat.me.is_some() && !self.chat.guilds.is_empty() {
                    Screen::Main
                } else if matches!(self.auth, AuthState::Validating) && self.qr.is_some() {
                    Screen::Login
                } else {
                    Screen::Splash
                }
            }
        }
    }

    pub fn open_channel(&self) -> Option<ChannelId> {
        match self.view {
            View::Home(c) => c,
            View::Guild { channel, .. } => channel,
        }
    }

    pub fn title(&self) -> String {
        let ch = self
            .open_channel()
            .and_then(|c| self.chat.channels.get(&c))
            .map(|c| match c.kind {
                ChannelKind::Dm | ChannelKind::GroupDm => c.label(|u| self.chat.user_name(u)),
                _ => format!("#{}", c.name.as_deref().unwrap_or("")),
            });
        match ch {
            Some(c) => format!("{c} - Ferrocord"),
            None => "Ferrocord".into(),
        }
    }

    fn ctx(&self) -> Ctx {
        Ctx {
            now_ms: now_ms(),
            open_channel: self.open_channel(),
            window_focused: self.window_focused,
            notification_preview: self.cfg.notification_preview,
        }
    }

    pub fn update(&mut self, msg: Message) -> Task<Message> {
        let task = match msg {
            Message::Ui(u) => self.update_ui(u),
            Message::Net(ev) => self.on_net(ev),
            Message::Done(d) => self.on_done(d),
        };
        if !self.first_frame_logged
            && self.cfg.trace_startup
            && self.fonts_ready
            && self.window_opened
        {
            self.first_frame_logged = true;
            tracing::info!(
                first_frame_ms = self.boot_at.elapsed().as_millis() as u64,
                "startup"
            );
        }
        task
    }

    // -- UI -------------------------------------------------------------------

    fn update_ui(&mut self, u: Ui) -> Task<Message> {
        match u {
            Ui::TokenInput(s) => {
                self.token_input = s;
                Task::none()
            }
            Ui::SubmitToken => match normalize_token(&self.token_input) {
                Err(e) => {
                    self.auth = AuthState::LoggedOut {
                        error: Some(e.into()),
                    };
                    Task::none()
                }
                Ok(t) => {
                    self.token_input.clear();
                    self.auth = self.auth.apply(AuthEvent::Submitted);
                    self.validate_token(Secret::new(t))
                }
            },
            Ui::RetryQr => {
                self.qr_generation += 1;
                self.qr = Some(QrState::Connecting);
                Task::none()
            }
            Ui::SelectHome => {
                let last_dm = self.chat.dm_list().first().map(|c| c.id);
                let target = match self.view {
                    View::Home(c) => c,
                    _ => None,
                }
                .or(last_dm);
                self.switch_view(View::Home(target))
            }
            Ui::SelectGuild(g) => {
                if let View::Guild { guild, .. } = self.view
                    && guild == g
                {
                    return Task::none();
                }
                let remembered = self
                    .store
                    .meta(&format!("guild_channel:{g}"))
                    .ok()
                    .flatten()
                    .and_then(|s| s.parse().ok())
                    .filter(|c| self.chat.channels.contains_key(c));
                let channel = remembered.or_else(|| self.chat.default_channel(g));
                self.switch_view(View::Guild { guild: g, channel })
            }
            Ui::SelectChannel(c) => {
                let view = match self.chat.channels.get(&c) {
                    Some(ch) if ch.kind.is_private() => View::Home(Some(c)),
                    Some(ch) => match ch.guild_id {
                        Some(g) => View::Guild {
                            guild: g,
                            channel: Some(c),
                        },
                        None => return Task::none(),
                    },
                    None => return Task::none(),
                };
                if !self
                    .chat
                    .channels
                    .get(&c)
                    .is_some_and(|c| c.kind.is_textual())
                {
                    return Task::none();
                }
                self.switch_view(view)
            }
            Ui::ToggleCategory(c) => {
                if !self.collapsed.remove(&c) {
                    self.collapsed.insert(c);
                }
                Task::none()
            }
            Ui::ComposerChanged(s) => {
                self.composer = s;
                let Some(c) = self.open_channel() else {
                    return Task::none();
                };
                let now = now_ms();
                let last = self.last_typing_sent.get(&c).copied().unwrap_or(0);
                if self.cfg.mock
                    || self.composer.is_empty()
                    || now.saturating_sub(last) < TYPING_INTERVAL_MS
                {
                    return Task::none();
                }
                self.last_typing_sent.insert(c, now);
                let rest = self.rest.clone();
                Task::perform(async move { rest.trigger_typing(c).await }, |_| {
                    Message::Done(Done::Ignored)
                })
            }
            Ui::Send => self.send(),
            Ui::Scrolled(vp) => {
                let from_top = vp.absolute_offset_reversed().y;
                if from_top < 400.0 {
                    return self.load_older();
                }
                Task::none()
            }
            Ui::RevealSpoiler(m) => {
                self.revealed_spoilers.insert(m);
                Task::none()
            }
            Ui::OpenUrl(url) => {
                if !(url.starts_with("https://") || url.starts_with("http://")) || self.cfg.mock {
                    return Task::none();
                }
                Task::perform(open_url(url), |_| Message::Done(Done::Ignored))
            }
            Ui::Logout => self.logout(),
            Ui::WindowFocus(f) => {
                self.window_focused = f;
                if f && let Some(c) = self.open_channel() {
                    return self.mark_read(c);
                }
                Task::none()
            }
            Ui::WindowOpened => {
                self.window_opened = true;
                Task::none()
            }
            Ui::Tick => {
                self.chat.prune_typing(now_ms());
                Task::none()
            }
        }
    }

    fn switch_view(&mut self, view: View) -> Task<Message> {
        if let Some(prev) = self.open_channel() {
            self.chat.trim_inactive(prev);
        }
        self.view = view;
        self.composer.clear();
        let (g, c) = match view {
            View::Home(c) => (None, c),
            View::Guild { guild, channel } => (Some(guild), channel),
        };
        let store = self.store.clone();
        let persist = Task::perform(
            async move {
                store
                    .blocking(move |s| {
                        s.set_meta(
                            "selected_guild",
                            &g.map(|g| g.to_string()).unwrap_or_default(),
                        )?;
                        s.set_meta(
                            "selected_channel",
                            &c.map(|c| c.to_string()).unwrap_or_default(),
                        )?;
                        if let (Some(g), Some(c)) = (g, c) {
                            s.set_meta(&format!("guild_channel:{g}"), &c.to_string())?;
                        }
                        Ok(())
                    })
                    .await
            },
            |_| Message::Done(Done::Ignored),
        );
        let Some(c) = c else { return persist };
        Task::batch([
            persist,
            self.mark_read(c),
            self.load_channel(c),
            self.request_images(),
        ])
    }

    fn mark_read(&mut self, c: ChannelId) -> Task<Message> {
        let Some(mid) = self.chat.mark_read(c) else {
            return Task::none();
        };
        let rs = self.chat.read_states.get(&c).copied().unwrap_or_default();
        let store = self.store.clone();
        let persist = Task::perform(
            async move { store.blocking(move |s| s.set_read_state(c, rs)).await },
            |_| Message::Done(Done::Ignored),
        );
        if self.cfg.mock {
            return persist;
        }
        let rest = self.rest.clone();
        Task::batch([
            persist,
            Task::perform(async move { rest.ack(c, mid).await }, |r| {
                if let Err(e) = r {
                    tracing::debug!(error = %e, "ack failed");
                }
                Message::Done(Done::Ignored)
            }),
        ])
    }

    /// Opens a channel's history: SQLite first, then the network.
    fn load_channel(&mut self, c: ChannelId) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let w = self.chat.messages.entry(c).or_default();
        if w.synced || w.loading {
            return Task::none();
        }
        w.loading = true;
        let cached = if w.messages.is_empty() {
            let store = self.store.clone();
            Task::perform(
                async move {
                    store
                        .blocking(move |s| s.messages(c, None, state::MESSAGE_WINDOW))
                        .await
                },
                move |r| {
                    Message::Done(Done::CachedHistory {
                        channel: c,
                        messages: r.unwrap_or_default(),
                    })
                },
            )
        } else {
            Task::none()
        };
        let rest = self.rest.clone();
        let fetch = Task::perform(
            async move { rest.messages(c, None, PAGE).await },
            move |r| {
                Message::Done(Done::History {
                    channel: c,
                    older: false,
                    result: r.map_err(|e| e.user_message()),
                })
            },
        );
        Task::batch([cached, fetch])
    }

    fn load_older(&mut self) -> Task<Message> {
        let Some(c) = self.open_channel() else {
            return Task::none();
        };
        if self.cfg.mock {
            return Task::none();
        }
        let Some(w) = self.chat.messages.get_mut(&c) else {
            return Task::none();
        };
        if w.loading || w.reached_start || !w.synced {
            return Task::none();
        }
        let Some(before) = w.oldest() else {
            return Task::none();
        };
        w.loading = true;
        let store = self.store.clone();
        let rest = self.rest.clone();
        Task::perform(
            async move {
                // Older pages come from SQLite when available.
                let local = store
                    .blocking(move |s| s.messages(c, Some(before), PAGE as usize))
                    .await
                    .unwrap_or_default();
                if local.len() == PAGE as usize {
                    return Ok(local);
                }
                rest.messages(c, Some(before), PAGE).await
            },
            move |r| {
                Message::Done(Done::History {
                    channel: c,
                    older: true,
                    result: r.map_err(|e| e.user_message()),
                })
            },
        )
    }

    fn send(&mut self) -> Task<Message> {
        let content = self.composer.trim().to_owned();
        let Some(c) = self.open_channel() else {
            return Task::none();
        };
        if content.is_empty() || content.chars().count() > 2000 {
            if !content.is_empty() {
                self.toast = Some("Messages can be at most 2000 characters.".into());
            }
            return Task::none();
        }
        self.composer.clear();
        let now = now_ms();
        let nonce = Snowflake::from_timestamp_ms(now, fastrand::u64(0..4096)).to_string();
        let Some(pending) = self
            .chat
            .push_pending(c, content.clone(), nonce.clone(), now)
        else {
            return Task::none();
        };
        self.last_typing_sent.remove(&c);
        if self.cfg.mock {
            let mut echo = pending;
            echo.pending = false;
            echo.id = Snowflake::from_timestamp_ms(now + 1, 0);
            return Task::perform(
                async move {
                    tokio::time::sleep(Duration::from_millis(150)).await;
                    echo
                },
                |m| Message::Net(Event::MessageCreate(Box::new(m))),
            );
        }
        tracing::info!(channel = %c, body = %Redacted(&content), "sending message");
        let rest = self.rest.clone();
        let n2 = nonce.clone();
        Task::perform(
            async move { rest.send_message(c, &content, &nonce).await },
            move |r| {
                Message::Done(Done::Sent {
                    channel: c,
                    nonce: n2.clone(),
                    result: r.map(Box::new).map_err(|e| e.user_message()),
                })
            },
        )
    }

    fn validate_token(&mut self, token: Secret) -> Task<Message> {
        let rest = self.rest.with_token(token.clone());
        self.token = Some(token);
        Task::perform(async move { rest.me().await }, |r| {
            let revoked = r.as_ref().err().is_some_and(|e| e.is_session_revoked());
            Message::Done(Done::TokenValidated {
                result: r.map_err(|e| e.user_message()),
                revoked,
            })
        })
    }

    fn start_gateway(&mut self) {
        self.gateway_generation += 1;
        if let Some(t) = &self.token {
            self.rest = self.rest.with_token(t.clone());
        }
    }

    fn logout(&mut self) -> Task<Message> {
        self.token = None;
        self.gateway_generation += 1;
        self.auth = self.auth.apply(AuthEvent::LoggedOut);
        self.connection = ConnectionState::Disconnected;
        self.chat = ChatState::default();
        self.view = View::Home(None);
        self.qr = None;
        if self.cfg.mock {
            return Task::none();
        }
        if let Ok(r) = RestClient::new(&self.cfg.api_url, &self.cfg.cdn_url, None) {
            self.rest = r;
        }
        let s = self.secrets.clone();
        let store = self.store.clone();
        Task::batch([
            Task::perform(secrets::clear_token(s), |_| Message::Done(Done::Ignored)),
            Task::perform(async move { store.blocking(|s| s.clear()).await }, |_| {
                Message::Done(Done::Ignored)
            }),
        ])
    }

    // -- network events ---------------------------------------------------------

    fn on_net(&mut self, ev: Event) -> Task<Message> {
        let mut tasks = Vec::new();
        match &ev {
            Event::Connection(s) => self.connection = *s,
            Event::AuthFailed => {
                self.auth = self.auth.apply(AuthEvent::Revoked);
                return self.logout_revoked();
            }
            Event::Fatal(reason) => {
                self.toast = Some(format!("Disconnected: {reason}"));
            }
            Event::Ready(r) => {
                if let Some(me) = &r.me {
                    self.auth = self.auth.apply(AuthEvent::Validated(me.clone()));
                }
                let store = self.store.clone();
                let ready = (**r).clone();
                let order: Vec<GuildId> = if ready.guild_order.is_empty() {
                    ready.guilds.iter().map(|g| g.id).collect()
                } else {
                    ready.guild_order.clone()
                };
                tasks.push(Task::perform(
                    async move { store.blocking(move |s| s.save_ready(&ready, &order)).await },
                    |r| {
                        if let Err(e) = r {
                            tracing::warn!(error = %e, "could not cache READY");
                        }
                        Message::Done(Done::Ignored)
                    },
                ));
            }
            Event::MessageCreate(m) => {
                let store = self.store.clone();
                let m2 = (**m).clone();
                if !self.cfg.mock {
                    tasks.push(Task::perform(
                        async move {
                            store
                                .blocking(move |s| {
                                    s.upsert_messages(std::slice::from_ref(&m2))?;
                                    s.prune_channel(
                                        m2.channel_id,
                                        crate::store::MAX_MESSAGES_PER_CHANNEL,
                                    )
                                    .map(|_| ())
                                })
                                .await
                        },
                        |_| Message::Done(Done::Ignored),
                    ));
                }
            }
            Event::MessageUpdate(p) if !self.cfg.mock => {
                let store = self.store.clone();
                let p = (**p).clone();
                tasks.push(Task::perform(
                    async move { store.blocking(move |s| s.patch_message(&p)).await },
                    |_| Message::Done(Done::Ignored),
                ));
            }
            Event::MessageDelete { id, .. } if !self.cfg.mock => {
                let store = self.store.clone();
                let id = *id;
                tasks.push(Task::perform(
                    async move { store.blocking(move |s| s.delete_messages(&[id])).await },
                    |_| Message::Done(Done::Ignored),
                ));
            }
            Event::MessageDeleteBulk { ids, .. } if !self.cfg.mock => {
                let store = self.store.clone();
                let ids = ids.clone();
                tasks.push(Task::perform(
                    async move { store.blocking(move |s| s.delete_messages(&ids)).await },
                    |_| Message::Done(Done::Ignored),
                ));
            }
            Event::ChannelUpsert(c, _) if !self.cfg.mock => {
                let store = self.store.clone();
                let c = (**c).clone();
                tasks.push(Task::perform(
                    async move { store.blocking(move |s| s.upsert_channel(&c)).await },
                    |_| Message::Done(Done::Ignored),
                ));
            }
            Event::GuildUpsert {
                guild, channels, ..
            } if !self.cfg.mock => {
                let store = self.store.clone();
                let (g, cs) = ((**guild).clone(), channels.clone());
                tasks.push(Task::perform(
                    async move { store.blocking(move |s| s.upsert_guild(&g, &cs)).await },
                    |_| Message::Done(Done::Ignored),
                ));
            }
            _ => {}
        }
        let ctx = self.ctx();
        let effects = self.chat.apply_event(&ev, &ctx);
        if matches!(ev, Event::Ready(_)) {
            // Re-sync the open channel after (re)connect.
            if let Some(c) = self.open_channel() {
                if !self.chat.channels.contains_key(&c) {
                    self.view = View::Home(None);
                } else {
                    tasks.push(self.load_channel(c));
                }
            }
            tasks.push(self.request_images());
        }
        if matches!(ev, Event::MessageCreate(_)) {
            tasks.push(self.request_images());
        }
        for fx in effects {
            match fx {
                Effect::Notify(n) => tasks.push(notify(n, self.cfg.mock)),
                Effect::Ack(c, m) if !self.cfg.mock => {
                    let rest = self.rest.clone();
                    tasks.push(Task::perform(async move { rest.ack(c, m).await }, |_| {
                        Message::Done(Done::Ignored)
                    }));
                }
                Effect::Ack(..) => {}
            }
        }
        Task::batch(tasks)
    }

    fn logout_revoked(&mut self) -> Task<Message> {
        let auth = self.auth.clone();
        let t = self.logout();
        // Keep the "session revoked" message from `apply(Revoked)`.
        self.auth = auth;
        t
    }

    // -- task completions -------------------------------------------------------

    fn on_done(&mut self, d: Done) -> Task<Message> {
        match d {
            Done::Ignored => Task::none(),
            Done::FontsLoaded => {
                self.fonts_ready = true;
                if let Some((_, _)) = &self.cfg.screenshot {
                    return Task::perform(
                        async { tokio::time::sleep(Duration::from_millis(600)).await },
                        |_| Message::Done(Done::ScreenshotReady),
                    );
                }
                if matches!(self.auth, AuthState::LoggedOut { .. })
                    && self.qr.is_none()
                    && !self.cfg.mock
                {
                    self.qr = Some(QrState::Connecting);
                }
                Task::none()
            }
            Done::ScreenshotReady => iced::window::latest()
                .and_then(iced::window::screenshot)
                .map(|s| Message::Done(Done::Screenshot(s))),
            Done::Screenshot(shot) => {
                if let Some((_, path)) = &self.cfg.screenshot {
                    match save_png(path, &shot) {
                        Ok(()) => tracing::info!(path = %path.display(), "screenshot saved"),
                        Err(e) => tracing::error!(error = %e, "screenshot failed"),
                    }
                }
                iced::exit()
            }
            Done::TokenRestored(Ok(Some(token))) => {
                self.auth = self.auth.apply(AuthEvent::Restored { found: true });
                self.start_gateway_with(token)
            }
            Done::TokenRestored(Ok(None)) => {
                self.auth = self.auth.apply(AuthEvent::Restored { found: false });
                if self.fonts_ready {
                    self.qr = Some(QrState::Connecting);
                }
                Task::none()
            }
            Done::TokenRestored(Err(e)) => {
                self.auth = self.auth.apply(AuthEvent::KeyringUnavailable(e));
                Task::none()
            }
            Done::TokenValidated { result, revoked } => match result {
                Ok(user) => {
                    let first_login = matches!(self.auth, AuthState::Validating);
                    self.auth = self.auth.apply(AuthEvent::Validated(user));
                    self.qr = None;
                    if first_login && self.gateway_generation == 0
                        || self.connection == ConnectionState::Disconnected
                    {
                        self.start_gateway();
                    }
                    let (s, t) = (self.secrets.clone(), self.token.clone());
                    match t {
                        Some(t) => Task::perform(secrets::save_token(s, t), |r| {
                            Message::Done(Done::TokenSaved(r.map_err(|e| e.user_message())))
                        }),
                        None => Task::none(),
                    }
                }
                Err(e) if revoked => {
                    self.auth = self.auth.apply(AuthEvent::Revoked);
                    let _ = e;
                    self.logout_revoked()
                }
                Err(e) => {
                    if matches!(self.auth, AuthState::Validating) && self.gateway_generation == 0 {
                        self.auth = self.auth.apply(AuthEvent::Rejected(e));
                        self.token = None;
                    } else {
                        // Restored session and the network is down: keep the
                        // cached UI; the gateway keeps retrying.
                        self.toast = Some(e);
                    }
                    Task::none()
                }
            },
            Done::TokenSaved(r) => {
                if let Err(e) = r {
                    self.toast = Some(format!("Could not save your session: {e}"));
                }
                Task::none()
            }
            Done::Qr(ev) => self.on_qr(ev),
            Done::CachedHistory { channel, messages } => {
                if !messages.is_empty() {
                    let full = messages.len() >= state::MESSAGE_WINDOW;
                    self.chat.insert_history(channel, messages, false, full);
                    if let Some(w) = self.chat.messages.get_mut(&channel) {
                        // Still waiting for the network page.
                        w.loading = true;
                        w.reached_start = false;
                    }
                }
                self.request_images()
            }
            Done::History {
                channel,
                older,
                result,
            } => match result {
                Ok(msgs) => {
                    let full = msgs.len() >= PAGE as usize;
                    let store = self.store.clone();
                    let to_store = msgs.clone();
                    self.chat.insert_history(channel, msgs, older, full);
                    if let Some(w) = self.chat.messages.get_mut(&channel) {
                        w.synced = true;
                    }
                    Task::batch([
                        Task::perform(
                            async move { store.blocking(move |s| s.upsert_messages(&to_store)).await },
                            |_| Message::Done(Done::Ignored),
                        ),
                        self.request_images(),
                    ])
                }
                Err(e) => {
                    if let Some(w) = self.chat.messages.get_mut(&channel) {
                        w.loading = false;
                    }
                    self.toast = Some(e);
                    Task::none()
                }
            },
            Done::Sent {
                channel,
                nonce,
                result,
            } => {
                match result {
                    Ok(m) => {
                        let ctx = self.ctx();
                        self.chat.apply_event(&Event::MessageCreate(m), &ctx);
                    }
                    Err(e) => {
                        // Mark every pending message in this channel that
                        // matches (or all pending ones if the nonce is lost).
                        if nonce.is_empty() {
                            let pend: Vec<String> = self
                                .chat
                                .messages
                                .get(&channel)
                                .map(|w| {
                                    w.messages
                                        .iter()
                                        .filter(|m| m.pending)
                                        .filter_map(|m| m.nonce.clone())
                                        .collect()
                                })
                                .unwrap_or_default();
                            for n in pend {
                                self.chat.mark_failed(channel, &n);
                            }
                        } else {
                            self.chat.mark_failed(channel, &nonce);
                        }
                        self.toast = Some(e);
                    }
                }
                Task::none()
            }
            Done::Image { key, bytes } => {
                match bytes {
                    Some(b) => self.images.insert_bytes(key, b),
                    None => self.images.mark_failed(&key),
                }
                Task::none()
            }
        }
    }

    fn start_gateway_with(&mut self, token: Secret) -> Task<Message> {
        self.token = Some(token.clone());
        self.start_gateway();
        self.validate_token(token)
    }

    fn on_qr(&mut self, ev: RaEvent) -> Task<Message> {
        match ev {
            RaEvent::QrReady(url) => self.qr = Some(qr_ready(&url)),
            RaEvent::Scanned { username } => self.qr = Some(QrState::Scanned { username }),
            RaEvent::Token(t) => {
                self.qr = None;
                self.auth = self.auth.apply(AuthEvent::Submitted);
                return self.validate_token(t);
            }
            RaEvent::Cancelled => {
                self.qr_generation += 1;
                self.qr = Some(QrState::Connecting);
            }
            RaEvent::Expired => self.qr = Some(QrState::Failed("The QR code expired.".into())),
            RaEvent::Error(e) => self.qr = Some(QrState::Failed(e)),
        }
        Task::none()
    }

    /// Fetches avatars / icons / attachment previews that are visible now.
    fn request_images(&mut self) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let mut urls: Vec<String> = Vec::new();
        for g in &self.chat.guilds {
            if let Some(u) = self.rest.guild_icon_url(g, 96) {
                urls.push(u);
            }
        }
        if let Some(c) = self.open_channel()
            && let Some(w) = self.chat.messages.get(&c)
        {
            for m in w.messages.iter().rev().take(60) {
                if let Some(u) = self.rest.avatar_url(&m.author, 80) {
                    urls.push(u);
                }
                for a in m.attachments.iter().filter(|a| a.is_image()) {
                    urls.push(attachment_preview_url(a));
                }
                if m.content.contains('<') {
                    let blocks = markdown::parse(&m.content);
                    if markdown::is_emoji_only(&blocks)
                        && let Some(markdown::Block::Paragraph(v)) = blocks.first()
                    {
                        for i in v {
                            if let markdown::Inline::Emoji { id, .. } = i {
                                urls.push(emoji_url(&self.cfg.cdn_url, *id));
                            }
                        }
                    }
                }
                for e in &m.embeds {
                    if let Some(i) = e.image.as_ref().or(e.thumbnail.as_ref()) {
                        urls.push(i.url.clone());
                    }
                }
            }
        }
        if let View::Home(_) = self.view {
            for c in self.chat.dm_list().into_iter().take(40) {
                if let Some(u) = c.recipients.first().and_then(|u| self.chat.users.get(u))
                    && let Some(url) = self.rest.avatar_url(u, 64)
                {
                    urls.push(url);
                }
            }
        }
        if let Some(me) = &self.chat.me
            && let Some(u) = self.rest.avatar_url(me, 64)
        {
            urls.push(u);
        }
        let tasks: Vec<Task<Message>> = urls
            .into_iter()
            .filter(|u| self.images.want(u))
            .take(64)
            .map(|url| {
                let rest = self.rest.clone();
                Task::perform(
                    async move {
                        let bytes = rest.fetch_bytes(&url).await.ok();
                        (url, bytes)
                    },
                    |(key, bytes)| Message::Done(Done::Image { key, bytes }),
                )
            })
            .collect();
        Task::batch(tasks)
    }

    pub fn image(&self, key: &str) -> Option<&iced::widget::image::Handle> {
        self.images.get(key)
    }

    pub fn avatar_key(&self, user: &User) -> Option<String> {
        self.rest
            .avatar_url(user, 80)
            .or_else(|| self.rest.avatar_url(user, 64))
    }

    pub fn avatar_handle(&self, user: &User) -> Option<&iced::widget::image::Handle> {
        let a = self.rest.avatar_url(user, 80)?;
        self.images.get(&a).or_else(|| {
            self.rest
                .avatar_url(user, 64)
                .and_then(|b| self.images.get(&b))
        })
    }

    pub fn guild_icon(&self, g: &Guild) -> Option<&iced::widget::image::Handle> {
        self.images.get(&self.rest.guild_icon_url(g, 96)?)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![iced::event::listen_with(|e, _, _| match e {
            iced::Event::Window(iced::window::Event::Focused) => {
                Some(Message::Ui(Ui::WindowFocus(true)))
            }
            iced::Event::Window(iced::window::Event::Opened { .. }) => {
                Some(Message::Ui(Ui::WindowOpened))
            }
            iced::Event::Window(iced::window::Event::Unfocused) => {
                Some(Message::Ui(Ui::WindowFocus(false)))
            }
            _ => None,
        })];
        if self.cfg.mock {
            return Subscription::batch(subs);
        }
        if let (Some(token), true) = (&self.token, self.gateway_generation > 0)
            && !matches!(self.auth, AuthState::LoggedOut { .. })
        {
            let key = Keyed {
                id: self.gateway_generation,
                value: GatewayConfig {
                    url: self.cfg.gateway_url.clone(),
                    token: token.clone(),
                    zlib: self.cfg.zlib,
                    properties: ClientProperties::default(),
                },
            };
            subs.push(
                Subscription::run_with(key, |k| gateway::spawn(k.value.clone())).map(Message::Net),
            );
        }
        let qr_active = matches!(
            self.qr,
            Some(QrState::Connecting | QrState::Ready { .. } | QrState::Scanned { .. })
        ) && matches!(self.auth, AuthState::LoggedOut { .. });
        if qr_active {
            let key = Keyed {
                id: self.qr_generation,
                value: (self.cfg.remote_auth_url.clone(), self.rest.clone()),
            };
            subs.push(
                Subscription::run_with(key, |k| {
                    remote_auth::spawn(k.value.0.clone(), k.value.1.clone())
                })
                .map(|e| Message::Done(Done::Qr(e))),
            );
        }
        if !self.chat.typing.is_empty() {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Ui(Ui::Tick)));
        }
        Subscription::batch(subs)
    }
}

/// Subscription identity: only the generation counter is hashed, so the
/// secret never feeds a hash and a new generation restarts the stream.
struct Keyed<T> {
    id: u64,
    value: T,
}

impl<T> Hash for Keyed<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::any::type_name::<T>().hash(state);
        self.id.hash(state);
    }
}

/// CDN URL of a custom emoji (static PNG; animated ones show frame 1).
pub fn emoji_url(cdn: &str, id: Snowflake) -> String {
    format!("{}/emojis/{id}.png?size=96", cdn.trim_end_matches('/'))
}

fn qr_ready(url: &str) -> QrState {
    match remote_auth::qr_matrix(url) {
        Ok((size, modules)) => QrState::Ready {
            url: url.to_owned(),
            size,
            modules,
        },
        Err(e) => QrState::Failed(e.user_message()),
    }
}

/// Discord's media proxy can resize previews; ask for a bounded size.
fn attachment_preview_url(a: &Attachment) -> String {
    let base = a.proxy_url.clone().unwrap_or_else(|| a.url.clone());
    if base.starts_with("mock://") {
        return base;
    }
    let (w, h) = crate::ui::fit_size(a.width.unwrap_or(400), a.height.unwrap_or(300), 400, 300);
    let sep = if base.contains('?') { '&' } else { '?' };
    format!("{base}{sep}width={w}&height={h}")
}

pub fn preview_key(a: &Attachment) -> String {
    attachment_preview_url(a)
}

/// Notification servers (KDE, GNOME) interpret a subset of HTML in the
/// summary/body. Message text is untrusted, so `&`, `<`, `>` (and quotes)
/// are escaped to keep it literal.
pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn notify(n: state::Notification, mock: bool) -> Task<Message> {
    if mock {
        return Task::none();
    }
    Task::perform(
        async move {
            let _ = tokio::task::spawn_blocking(move || {
                let r = notify_rust::Notification::new()
                    .appname(crate::config::APP_NAME)
                    .summary(&escape_markup(&n.title))
                    .body(&escape_markup(&n.body))
                    .icon("dev.ferro.ferrocord")
                    .hint(notify_rust::Hint::DesktopEntry(
                        crate::config::APP_ID.into(),
                    ))
                    .show();
                if let Err(e) = r {
                    tracing::debug!(error = %e, "notification failed");
                }
            })
            .await;
        },
        |_| Message::Done(Done::Ignored),
    )
}

async fn open_url(url: String) {
    let _ = tokio::task::spawn_blocking(move || {
        #[cfg(target_os = "linux")]
        let cmd = "xdg-open";
        #[cfg(target_os = "macos")]
        let cmd = "open";
        #[cfg(target_os = "windows")]
        let cmd = "explorer";
        #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
        let cmd = "xdg-open";
        let _ = std::process::Command::new(cmd).arg(url).spawn();
    })
    .await;
}

fn save_png(path: &std::path::Path, shot: &iced::window::Screenshot) -> crate::error::Result<()> {
    let img = image::RgbaImage::from_raw(shot.size.width, shot.size.height, shot.rgba.to_vec())
        .ok_or_else(|| crate::error::Error::Io(std::io::Error::other("bad screenshot buffer")))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    img.save_with_format(path, image::ImageFormat::Png)
        .map_err(|e| crate::error::Error::Io(std::io::Error::other(e.to_string())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notification_text_is_escaped() {
        assert_eq!(
            escape_markup(r#"<b>hi</b> & <a href="x">'y'</a>"#),
            "&lt;b&gt;hi&lt;/b&gt; &amp; &lt;a href=&quot;x&quot;&gt;&#39;y&#39;&lt;/a&gt;"
        );
        assert_eq!(escape_markup("plain 🦀"), "plain 🦀");
    }
}
