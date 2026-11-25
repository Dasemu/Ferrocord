//! Voice channels and DM / group calls: who is in which channel, our own
//! connection (op 4 -> VOICE_STATE/SERVER_UPDATE -> voice gateway), mute
//! and deafen, the call ring / accept / decline state machine, and the
//! local voice settings (devices, sensitivity, per-user volume).
//!
//! [`Voice`] is a pure reducer; the `impl App` block at the bottom wires it
//! to tasks, the gateway handle and the voice connection subscription.

use std::collections::{HashMap, HashSet};

use iced::{Subscription, Task};
use serde::{Deserialize, Serialize};

use super::{App, Done, Keyed, Message};
use crate::net::Event;
use crate::net::gateway::VoiceRequest;
use crate::net::model::*;
use crate::net::voice::audio::DeviceChoice;
use crate::net::voice::conn::VoiceConfig;
use crate::net::voice::session::Identity;
use crate::net::voice::{VoiceControl, VoiceEvent};

/// Voice states remembered at most (all servers plus calls).
const MAX_MEMBERS: usize = 20_000;
/// SQLite `meta` key of the voice settings (kept on logout like `pref.*`).
pub const PREF_KEY: &str = "pref.voice";

/// User input for voice.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceUi {
    Join(ChannelId),
    Leave,
    ToggleMute,
    ToggleDeafen,
    /// Phone button in a DM header: join the call and ring the others.
    StartCall(ChannelId),
    AcceptCall(ChannelId),
    DeclineCall(ChannelId),
    /// Right-click on a user in a voice channel / call.
    UserMenu(UserId),
    CloseUserMenu,
    /// Volume in percent (0..=200).
    UserVolume(UserId, f32),
    ToggleUserMute(UserId),
    InputDevice(Option<String>),
    OutputDevice(Option<String>),
    /// Voice activity threshold in dB (-100..=0).
    Threshold(f32),
    /// Microphone volume in percent (0..=200).
    InputVolume(f32),
    /// Speaker volume in percent (0..=200).
    OutputVolume(f32),
    RefreshDevices,
}

/// Task completions for voice.
#[derive(Debug, Clone)]
pub enum VoiceDone {
    Devices(Vec<String>, Vec<String>),
    Rang(Result<(), String>),
    Declined(Result<(), String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Op 4 sent; waiting for VOICE_STATE_UPDATE + VOICE_SERVER_UPDATE.
    Requesting,
    /// Voice gateway handshake running.
    Connecting,
    Connected,
    Reconnecting,
}

/// Our own voice connection.
#[derive(Clone, PartialEq)]
pub struct Connection {
    pub guild: Option<GuildId>,
    pub channel: ChannelId,
    pub phase: Phase,
    session_id: Option<String>,
    server: Option<VoiceServer>,
    pub ping: Option<u32>,
    pub e2ee: bool,
    pub since_ms: u64,
    /// Subscription identity; bumped for every (re)connection.
    pub generation: u64,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("guild", &self.guild)
            .field("channel", &self.channel)
            .field("phase", &self.phase)
            .finish_non_exhaustive()
    }
}

impl Connection {
    /// Ready to start the voice gateway (both halves arrived).
    fn ready(&self) -> bool {
        self.session_id.is_some() && self.server.as_ref().is_some_and(|s| s.endpoint.is_some())
    }
}

/// Persisted voice settings (like the official "Voice & Video" page).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VoicePrefs {
    #[serde(default)]
    pub input: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default = "default_threshold")]
    pub threshold_db: f32,
    #[serde(default)]
    pub self_mute: bool,
    #[serde(default)]
    pub self_deaf: bool,
    /// user -> (volume percent, locally muted).
    #[serde(default)]
    pub users: HashMap<UserId, (f32, bool)>,
    /// "Input Volume" in percent (0..=200).
    #[serde(default = "full_volume")]
    pub input_volume: f32,
    /// "Output Volume" in percent (0..=200).
    #[serde(default = "full_volume")]
    pub output_volume: f32,
}

fn full_volume() -> f32 {
    100.0
}

fn default_threshold() -> f32 {
    -60.0
}

impl Default for VoicePrefs {
    fn default() -> Self {
        Self {
            input: None,
            output: None,
            threshold_db: default_threshold(),
            self_mute: false,
            self_deaf: false,
            users: HashMap::new(),
            input_volume: full_volume(),
            output_volume: full_volume(),
        }
    }
}

/// Open per-user menu (volume slider, local mute).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct UserMenu {
    pub user: UserId,
    pub at: iced::Point,
}

/// Side effects for the app glue.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceEffect {
    /// Send op 4.
    Gateway(VoiceRequest),
    /// Ring the recipients of a call.
    Ring(ChannelId),
    /// Incoming call notification.
    NotifyIncoming(ChannelId),
    Toast(String),
}

#[derive(Debug, Clone, Default)]
pub struct Voice {
    /// Everybody's voice state, keyed by user.
    pub members: HashMap<UserId, VoiceState>,
    pub speaking: HashSet<UserId>,
    pub conn: Option<Connection>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub calls: HashMap<ChannelId, CallState>,
    /// A call ringing us (overlay + notification).
    pub incoming: Option<ChannelId>,
    declined: HashSet<ChannelId>,
    pub control: VoiceControl,
    pub prefs: VoicePrefs,
    pub devices: (Vec<String>, Vec<String>),
    pub user_menu: Option<UserMenu>,
    generation: u64,
}

impl Voice {
    pub fn with_prefs(prefs: VoicePrefs) -> Self {
        let mut v = Self {
            control: VoiceControl::new(),
            ..Default::default()
        };
        v.self_mute = prefs.self_mute;
        v.self_deaf = prefs.self_deaf;
        v.prefs = prefs;
        v.sync_control();
        v
    }

    /// Pushes settings into the shared control the engine reads.
    pub fn sync_control(&self) {
        let c = &self.control;
        c.set_self_mute(self.self_mute || self.self_deaf);
        c.set_self_deaf(self.self_deaf);
        c.set_threshold_db(self.prefs.threshold_db);
        c.set_volumes(self.prefs.input_volume, self.prefs.output_volume);
        c.set_devices(DeviceChoice {
            input: self.prefs.input.clone(),
            output: self.prefs.output.clone(),
        });
        for (u, (vol, muted)) in &self.prefs.users {
            c.set_user(u.0, vol / 100.0, *muted);
        }
    }

    /// Users in `channel`, in a stable order (by user id, as the server
    /// list is not ordered either).
    pub fn channel_members(&self, channel: ChannelId) -> Vec<&VoiceState> {
        let mut v: Vec<&VoiceState> = self
            .members
            .values()
            .filter(|s| s.channel_id == Some(channel))
            .collect();
        v.sort_by_key(|s| s.user_id);
        v
    }

    pub fn connected_channel(&self) -> Option<ChannelId> {
        self.conn.as_ref().map(|c| c.channel)
    }

    pub fn is_speaking(&self, user: UserId) -> bool {
        self.speaking.contains(&user)
    }

    pub fn user_volume(&self, user: UserId) -> (f32, bool) {
        self.prefs
            .users
            .get(&user)
            .copied()
            .unwrap_or((100.0, false))
    }

    fn request(&self) -> Option<VoiceRequest> {
        self.conn.as_ref().map(|c| VoiceRequest {
            guild: c.guild,
            channel: Some(c.channel),
            self_mute: self.self_mute || self.self_deaf,
            self_deaf: self.self_deaf,
        })
    }

    /// Joins a voice channel (`guild` set) or a DM call (`guild: None`).
    pub fn join(
        &mut self,
        guild: Option<GuildId>,
        channel: ChannelId,
        now_ms: u64,
    ) -> Vec<VoiceEffect> {
        if self.conn.as_ref().is_some_and(|c| c.channel == channel) {
            return Vec::new();
        }
        self.speaking.clear();
        self.generation += 1;
        self.conn = Some(Connection {
            guild,
            channel,
            phase: Phase::Requesting,
            session_id: None,
            server: None,
            ping: None,
            e2ee: false,
            since_ms: now_ms,
            generation: self.generation,
        });
        if self.incoming == Some(channel) {
            self.incoming = None;
        }
        self.request()
            .map(VoiceEffect::Gateway)
            .into_iter()
            .collect()
    }

    /// Leaves voice (op 4 with a null channel).
    pub fn leave(&mut self, me: Option<UserId>) -> Vec<VoiceEffect> {
        let Some(c) = self.conn.take() else {
            return Vec::new();
        };
        self.speaking.clear();
        if let Some(me) = me {
            self.members.remove(&me);
        }
        vec![VoiceEffect::Gateway(VoiceRequest {
            guild: c.guild,
            channel: None,
            self_mute: self.self_mute,
            self_deaf: self.self_deaf,
        })]
    }

    pub fn toggle_mute(&mut self) -> Vec<VoiceEffect> {
        if self.self_deaf {
            // Unmuting while deafened undeafens too (official behaviour).
            self.self_deaf = false;
            self.self_mute = false;
        } else {
            self.self_mute = !self.self_mute;
        }
        self.after_toggle()
    }

    pub fn toggle_deafen(&mut self) -> Vec<VoiceEffect> {
        self.self_deaf = !self.self_deaf;
        self.after_toggle()
    }

    fn after_toggle(&mut self) -> Vec<VoiceEffect> {
        self.prefs.self_mute = self.self_mute;
        self.prefs.self_deaf = self.self_deaf;
        self.sync_control();
        self.request()
            .map(VoiceEffect::Gateway)
            .into_iter()
            .collect()
    }

    fn call_rings_me(&self, call: &CallState, me: UserId) -> bool {
        call.ringing.contains(&me)
            && !self.declined.contains(&call.channel_id)
            && self.connected_channel() != Some(call.channel_id)
    }

    /// Main gateway events.
    pub fn apply(&mut self, ev: &Event, me: Option<UserId>) -> Vec<VoiceEffect> {
        let mut fx = Vec::new();
        match ev {
            Event::Ready(r) => {
                self.members.clear();
                self.calls.clear();
                for v in r.voice_states.iter().take(MAX_MEMBERS) {
                    self.members.insert(v.user_id, v.clone());
                }
            }
            Event::GuildUpsert {
                guild,
                voice_states,
                partial: false,
                ..
            } => {
                self.members.retain(|_, v| v.guild_id != Some(guild.id));
                for v in voice_states {
                    if self.members.len() < MAX_MEMBERS {
                        self.members.insert(v.user_id, v.clone());
                    }
                }
            }
            Event::GuildDelete(g) => self.members.retain(|_, v| v.guild_id != Some(*g)),
            Event::VoiceState(v) => {
                let mine = Some(v.user_id) == me;
                match v.channel_id {
                    Some(_) => {
                        if self.members.len() < MAX_MEMBERS || self.members.contains_key(&v.user_id)
                        {
                            self.members.insert(v.user_id, (**v).clone());
                        }
                    }
                    None => {
                        self.members.remove(&v.user_id);
                        self.speaking.remove(&v.user_id);
                    }
                }
                if mine {
                    match (&mut self.conn, v.channel_id) {
                        (Some(c), Some(ch)) if ch == c.channel => {
                            if c.session_id.as_deref() != Some(&v.session_id) {
                                c.session_id = Some(v.session_id.clone());
                            }
                        }
                        (Some(c), Some(ch)) => {
                            // Moved by a moderator: follow, like the official client.
                            c.channel = ch;
                            c.session_id = Some(v.session_id.clone());
                        }
                        (Some(c), None) if c.phase != Phase::Requesting => {
                            // Disconnected by the server or another session.
                            self.conn = None;
                            self.speaking.clear();
                        }
                        _ => {}
                    }
                    self.maybe_connect();
                }
            }
            Event::VoiceServer(s) => {
                if let Some(c) = &mut self.conn {
                    let matches = match c.guild {
                        Some(g) => s.guild_id == Some(g),
                        None => s.channel_id == Some(c.channel) || s.guild_id.is_none(),
                    };
                    if matches {
                        c.server = Some((**s).clone());
                        // A new server means a new voice session.
                        if c.phase != Phase::Requesting {
                            c.phase = Phase::Requesting;
                        }
                    }
                }
                self.maybe_connect();
            }
            Event::Call(call) => {
                for v in &call.voice_states {
                    if v.channel_id.is_some() && self.members.len() < MAX_MEMBERS {
                        self.members.insert(v.user_id, v.clone());
                    }
                }
                let rings = me.is_some_and(|m| self.call_rings_me(call, m));
                let was = self.incoming == Some(call.channel_id);
                self.calls.insert(call.channel_id, (**call).clone());
                if rings && !was && self.incoming.is_none() {
                    self.incoming = Some(call.channel_id);
                    fx.push(VoiceEffect::NotifyIncoming(call.channel_id));
                } else if !rings && was {
                    self.incoming = None;
                }
                if !call.ringing.iter().any(|u| Some(*u) == me) {
                    self.declined.remove(&call.channel_id);
                }
            }
            Event::CallDelete(ch) => {
                self.calls.remove(ch);
                self.declined.remove(ch);
                if self.incoming == Some(*ch) {
                    self.incoming = None;
                }
                self.members
                    .retain(|_, v| !(v.guild_id.is_none() && v.channel_id == Some(*ch)));
            }
            _ => {}
        }
        fx
    }

    fn maybe_connect(&mut self) {
        if let Some(c) = &mut self.conn
            && c.phase == Phase::Requesting
            && c.ready()
        {
            self.generation += 1;
            c.generation = self.generation;
            c.phase = Phase::Connecting;
        }
    }

    /// Events from the running voice connection.
    pub fn on_voice_event(&mut self, ev: &VoiceEvent, me: Option<UserId>) -> Vec<VoiceEffect> {
        let Some(c) = &mut self.conn else {
            return Vec::new();
        };
        match ev {
            VoiceEvent::Connected { e2ee } => {
                c.phase = Phase::Connected;
                c.e2ee = *e2ee;
            }
            VoiceEvent::Encryption { e2ee } => c.e2ee = *e2ee,
            VoiceEvent::Ping(ms) => c.ping = Some(*ms),
            VoiceEvent::Reconnecting => c.phase = Phase::Reconnecting,
            VoiceEvent::Speaking { user, speaking } => {
                let user = Snowflake(*user);
                if *speaking {
                    if self.speaking.len() < 1000 {
                        self.speaking.insert(user);
                    }
                } else {
                    self.speaking.remove(&user);
                }
            }
            VoiceEvent::Ended { reason, rejoin } => {
                self.speaking.clear();
                if *rejoin {
                    // Ask the gateway for a fresh voice server.
                    c.phase = Phase::Requesting;
                    c.server = None;
                    c.session_id = None;
                    if let Some(r) = self.request() {
                        return vec![
                            VoiceEffect::Gateway(VoiceRequest { channel: None, ..r }),
                            VoiceEffect::Gateway(r),
                        ];
                    }
                } else {
                    let mut fx = self.leave(me);
                    fx.push(VoiceEffect::Toast(format!("Voice disconnected: {reason}")));
                    return fx;
                }
            }
        }
        Vec::new()
    }

    /// Accepts an incoming call.
    pub fn accept(&mut self, channel: ChannelId, now_ms: u64) -> Vec<VoiceEffect> {
        self.join(None, channel, now_ms)
    }

    /// Declines: hides the overlay; the app tells the server to stop ringing.
    pub fn decline(&mut self, channel: ChannelId) {
        self.declined.insert(channel);
        if self.incoming == Some(channel) {
            self.incoming = None;
        }
    }

    /// Per-user volume (percent) / local mute.
    pub fn set_user(&mut self, user: UserId, volume: f32, muted: bool) {
        let volume = volume.clamp(0.0, 200.0);
        if (volume - 100.0).abs() < f32::EPSILON && !muted {
            self.prefs.users.remove(&user);
        } else if self.prefs.users.len() < 10_000 || self.prefs.users.contains_key(&user) {
            self.prefs.users.insert(user, (volume, muted));
        }
        self.control.set_user(user.0, volume / 100.0, muted);
    }

    /// An ongoing call in a DM / group (someone is in it).
    pub fn active_call(&self, channel: ChannelId) -> bool {
        self.calls.contains_key(&channel)
            || self
                .members
                .values()
                .any(|v| v.guild_id.is_none() && v.channel_id == Some(channel))
    }
}

// -- app glue ---------------------------------------------------------------

impl App {
    /// The 1:1 DM channel with `user`, if one exists (for "Call" in a
    /// profile popout).
    pub fn dm_with(&self, user: UserId) -> Option<ChannelId> {
        if self.chat.me_id() == Some(user) {
            return None;
        }
        self.chat
            .channels
            .values()
            .find(|c| c.kind == ChannelKind::Dm && c.recipients == [user])
            .map(|c| c.id)
    }

    pub(super) fn update_voice(&mut self, v: VoiceUi) -> Task<Message> {
        let now = super::now_ms();
        let me = self.chat.me_id();
        let fx = match v {
            VoiceUi::Join(ch) => {
                let Some(c) = self.chat.channels.get(&ch) else {
                    return Task::none();
                };
                if !matches!(c.kind, ChannelKind::Voice | ChannelKind::Stage) {
                    return Task::none();
                }
                let guild = c.guild_id;
                let fx = self.voice.join(guild, ch, now);
                self.mock_voice_join();
                fx
            }
            VoiceUi::Leave => self.voice.leave(me),
            VoiceUi::ToggleMute => self.voice.toggle_mute(),
            VoiceUi::ToggleDeafen => self.voice.toggle_deafen(),
            VoiceUi::StartCall(ch) => {
                if !self
                    .chat
                    .channels
                    .get(&ch)
                    .is_some_and(|c| c.kind.is_private())
                {
                    return Task::none();
                }
                // From a profile popout: close it, like the official client.
                if self.profiles.popout.is_some() {
                    self.profiles.popout = None;
                    self.anim.close_popout();
                }
                let already = self.voice.active_call(ch);
                let mut fx = self.voice.join(None, ch, now);
                // Joining an ongoing call does not ring again.
                if !already && !fx.is_empty() {
                    fx.push(VoiceEffect::Ring(ch));
                }
                self.mock_voice_join();
                fx
            }
            VoiceUi::AcceptCall(ch) => {
                let fx = self.voice.accept(ch, now);
                self.mock_voice_join();
                let mut t = vec![self.switch_view(super::View::Home(Some(ch)))];
                t.push(self.run_voice_effects(fx));
                return Task::batch(t);
            }
            VoiceUi::DeclineCall(ch) => {
                self.voice.decline(ch);
                if self.cfg.mock {
                    return Task::none();
                }
                let rest = self.rest.clone();
                let Some(me) = me else {
                    return Task::none();
                };
                return Task::perform(
                    async move {
                        rest.stop_ringing(ch, vec![me])
                            .await
                            .map_err(|e| e.user_message())
                    },
                    |r| Message::Done(Done::Voice(VoiceDone::Declined(r))),
                );
            }
            VoiceUi::UserMenu(u) => {
                let at = super::input::cursor();
                self.voice.user_menu = Some(UserMenu { user: u, at });
                Vec::new()
            }
            VoiceUi::CloseUserMenu => {
                self.voice.user_menu = None;
                Vec::new()
            }
            VoiceUi::UserVolume(u, vol) => {
                let (_, muted) = self.voice.user_volume(u);
                self.voice.set_user(u, vol, muted);
                return self.save_voice_prefs();
            }
            VoiceUi::ToggleUserMute(u) => {
                let (vol, muted) = self.voice.user_volume(u);
                self.voice.set_user(u, vol, !muted);
                return self.save_voice_prefs();
            }
            VoiceUi::InputDevice(d) => {
                self.voice.prefs.input = d;
                self.voice.sync_control();
                return self.save_voice_prefs();
            }
            VoiceUi::OutputDevice(d) => {
                self.voice.prefs.output = d;
                self.voice.sync_control();
                return self.save_voice_prefs();
            }
            VoiceUi::Threshold(db) => {
                self.voice.prefs.threshold_db = db.clamp(-100.0, 0.0);
                self.voice.sync_control();
                return self.save_voice_prefs();
            }
            VoiceUi::InputVolume(v) => {
                self.voice.prefs.input_volume = v.clamp(0.0, 200.0);
                self.voice.sync_control();
                return self.save_voice_prefs();
            }
            VoiceUi::OutputVolume(v) => {
                self.voice.prefs.output_volume = v.clamp(0.0, 200.0);
                self.voice.sync_control();
                return self.save_voice_prefs();
            }
            VoiceUi::RefreshDevices => {
                if self.cfg.mock {
                    self.voice.devices = (
                        vec!["Built-in Microphone".into(), "USB Headset".into()],
                        vec!["Built-in Speakers".into(), "USB Headset".into()],
                    );
                    return Task::none();
                }
                return Task::perform(
                    async {
                        tokio::task::spawn_blocking(crate::net::voice::audio::list_devices)
                            .await
                            .unwrap_or_default()
                    },
                    |(i, o)| Message::Done(Done::Voice(VoiceDone::Devices(i, o))),
                );
            }
        };
        // Mute / deafen (the only effects sending op 4 here besides join
        // and leave) are remembered across restarts.
        let save = fx.iter().any(|e| matches!(e, VoiceEffect::Gateway(_)));
        let t = self.run_voice_effects(fx);
        if save {
            Task::batch([t, self.save_voice_prefs()])
        } else {
            t
        }
    }

    /// In mock mode there is no server: pretend the join succeeded.
    fn mock_voice_join(&mut self) {
        if !self.cfg.mock {
            return;
        }
        let Some(me) = self.chat.me.clone() else {
            return;
        };
        if let Some(c) = &mut self.voice.conn {
            c.phase = Phase::Connected;
            c.e2ee = true;
            c.ping = Some(38);
            let vs = VoiceState {
                guild_id: c.guild,
                channel_id: Some(c.channel),
                user_id: me.id,
                self_mute: self.voice.self_mute || self.voice.self_deaf,
                self_deaf: self.voice.self_deaf,
                user: Some(me),
                ..Default::default()
            };
            self.voice.members.insert(vs.user_id, vs);
        }
    }

    fn run_voice_effects(&mut self, fx: Vec<VoiceEffect>) -> Task<Message> {
        let mut tasks = Vec::new();
        for e in fx {
            match e {
                VoiceEffect::Gateway(r) => {
                    if self.cfg.mock {
                        if let Some(me) = self.chat.me_id()
                            && let Some(v) = self.voice.members.get_mut(&me)
                        {
                            v.self_mute = r.self_mute;
                            v.self_deaf = r.self_deaf;
                        }
                    } else {
                        self.gateway.set_voice(r);
                    }
                }
                VoiceEffect::Ring(ch) => {
                    if !self.cfg.mock {
                        let rest = self.rest.clone();
                        tasks.push(Task::perform(
                            async move { rest.ring(ch, None).await.map_err(|e| e.user_message()) },
                            |r| Message::Done(Done::Voice(VoiceDone::Rang(r))),
                        ));
                    }
                }
                VoiceEffect::NotifyIncoming(ch) => {
                    let title = self
                        .chat
                        .channels
                        .get(&ch)
                        .map(|c| c.label(|u| self.chat.user_name(u)))
                        .unwrap_or_else(|| "Incoming call".into());
                    tasks.push(super::notify(
                        super::state::Notification {
                            title,
                            body: "Incoming call".into(),
                            channel_id: ch,
                            guild_id: None,
                        },
                        self.cfg.mock || self.prefs.muted,
                        self.settings.local.notify_sound,
                    ));
                }
                VoiceEffect::Toast(t) => self.toast = Some(t),
            }
        }
        Task::batch(tasks)
    }

    pub(super) fn save_voice_prefs(&self) -> Task<Message> {
        if self.cfg.mock {
            return Task::none();
        }
        let Ok(json) = serde_json::to_string(&self.voice.prefs) else {
            return Task::none();
        };
        let store = self.store.clone();
        Task::perform(
            async move { store.blocking(move |s| s.set_meta(PREF_KEY, &json)).await },
            |_| Message::Done(Done::Ignored),
        )
    }

    pub(super) fn on_voice_done(&mut self, d: VoiceDone) -> Task<Message> {
        match d {
            VoiceDone::Devices(i, o) => self.voice.devices = (i, o),
            VoiceDone::Rang(Err(e)) => self.toast = Some(format!("Could not ring: {e}")),
            VoiceDone::Declined(Err(e)) => tracing::debug!(error = %e, "stop ringing failed"),
            VoiceDone::Rang(Ok(())) | VoiceDone::Declined(Ok(())) => {}
        }
        Task::none()
    }

    pub(super) fn voice_on_gateway(&mut self, ev: &Event) -> Task<Message> {
        let me = self.chat.me_id();
        let fx = self.voice.apply(ev, me);
        self.run_voice_effects(fx)
    }

    pub(super) fn on_voice_event(&mut self, ev: VoiceEvent) -> Task<Message> {
        let me = self.chat.me_id();
        let fx = self.voice.on_voice_event(&ev, me);
        self.run_voice_effects(fx)
    }

    /// The voice connection runs only while we are joined and the voice
    /// server is known; nothing voice-related exists otherwise.
    pub(super) fn voice_subscription(&self) -> Option<Subscription<Message>> {
        if self.cfg.mock {
            return None;
        }
        let c = self.voice.conn.as_ref()?;
        if c.phase == Phase::Requesting {
            return None;
        }
        let server = c.server.as_ref()?;
        let endpoint = server.endpoint.clone()?;
        let me = self.chat.me_id()?;
        let cfg = VoiceConfig {
            endpoint,
            identity: Identity {
                server_id: c.guild.unwrap_or(c.channel).0,
                channel_id: c.channel.0,
                user_id: me.0,
                session_id: c.session_id.clone()?,
                token: crate::auth::secrets::Secret::new(server.token.clone()),
            },
            control: self.voice.control.clone(),
            audio: crate::net::voice::audio::cpal_factory(),
        };
        let key = Keyed {
            id: c.generation,
            value: cfg,
        };
        Some(
            Subscription::run_with(key, |k| crate::net::voice::conn::spawn(k.value.clone()))
                .map(Message::Voice),
        )
    }

    /// Sets up a mock voice screenshot.
    pub(super) fn mock_voice_screen(&mut self, s: crate::config::ScreenshotScreen) {
        use crate::config::ScreenshotScreen as S;
        use crate::mock;
        self.chat.me = Some(mock::user(1));
        match s {
            S::Voice => {
                self.view = super::View::Guild {
                    guild: mock::GUILD_RUST,
                    channel: Some(mock::CH_GENERAL),
                };
                self.chat.mark_read(mock::CH_GENERAL);
                let _ = self.update_voice(VoiceUi::Join(mock::VC_LOUNGE));
                self.voice.speaking.insert(Snowflake(3));
                self.voice.speaking.insert(Snowflake(1));
            }
            S::Call => {
                self.view = super::View::Home(Some(mock::DM_ALICE));
                self.chat.mark_read(mock::DM_ALICE);
                let _ = self.update_voice(VoiceUi::StartCall(mock::DM_ALICE));
                let alice = VoiceState {
                    channel_id: Some(mock::DM_ALICE),
                    user_id: Snowflake(2),
                    user: Some(mock::user(2)),
                    ..Default::default()
                };
                self.voice.members.insert(alice.user_id, alice);
                self.voice.speaking.insert(Snowflake(2));
            }
            _ => {
                self.view = super::View::Home(Some(mock::DM_BOB));
                self.chat.mark_read(mock::DM_BOB);
                let call = CallState {
                    channel_id: mock::DM_ALICE,
                    message_id: None,
                    ringing: vec![Snowflake(1)],
                    voice_states: vec![VoiceState {
                        channel_id: Some(mock::DM_ALICE),
                        user_id: Snowflake(2),
                        ..Default::default()
                    }],
                    unavailable: false,
                };
                let _ = self
                    .voice
                    .apply(&Event::Call(Box::new(call)), Some(Snowflake(1)));
            }
        }
    }

    /// Loads voice settings from SQLite `meta`.
    pub(super) fn load_voice_prefs(store: &crate::store::Store) -> VoicePrefs {
        store
            .meta(PREF_KEY)
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: UserId = Snowflake(1);
    const G: GuildId = Snowflake(100);
    const VC: ChannelId = Snowflake(140);
    const DM: ChannelId = Snowflake(600);

    fn vs(user: u64, guild: Option<GuildId>, ch: Option<ChannelId>) -> VoiceState {
        VoiceState {
            guild_id: guild,
            channel_id: ch,
            user_id: Snowflake(user),
            session_id: format!("s{user}"),
            ..Default::default()
        }
    }

    fn server(guild: Option<GuildId>, channel: Option<ChannelId>) -> Event {
        Event::VoiceServer(Box::new(VoiceServer {
            guild_id: guild,
            channel_id: channel,
            endpoint: Some("x.discord.media:443".into()),
            token: "t".into(),
        }))
    }

    #[test]
    fn join_waits_for_state_and_server() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        let fx = v.join(Some(G), VC, 0);
        assert_eq!(
            fx,
            vec![VoiceEffect::Gateway(VoiceRequest {
                guild: Some(G),
                channel: Some(VC),
                self_mute: false,
                self_deaf: false
            })]
        );
        assert_eq!(v.conn.as_ref().unwrap().phase, Phase::Requesting);
        // Joining the same channel again is a no-op.
        assert!(v.join(Some(G), VC, 0).is_empty());
        v.apply(&server(Some(G), None), Some(ME));
        assert_eq!(v.conn.as_ref().unwrap().phase, Phase::Requesting);
        v.apply(
            &Event::VoiceState(Box::new(vs(1, Some(G), Some(VC)))),
            Some(ME),
        );
        let c = v.conn.clone().unwrap();
        assert_eq!(c.phase, Phase::Connecting);
        assert_eq!(v.channel_members(VC).len(), 1);
        v.on_voice_event(&VoiceEvent::Connected { e2ee: true }, Some(ME));
        v.on_voice_event(&VoiceEvent::Ping(40), Some(ME));
        let c = v.conn.clone().unwrap();
        assert_eq!(
            (c.phase, c.ping, c.e2ee),
            (Phase::Connected, Some(40), true)
        );
        v.on_voice_event(
            &VoiceEvent::Speaking {
                user: 7,
                speaking: true,
            },
            Some(ME),
        );
        assert!(v.is_speaking(Snowflake(7)));
        // Leave: op 4 with a null channel, own state removed.
        let fx = v.leave(Some(ME));
        assert!(matches!(
            fx[0],
            VoiceEffect::Gateway(VoiceRequest { channel: None, .. })
        ));
        assert!(v.conn.is_none());
        assert!(v.channel_members(VC).is_empty());
        assert!(v.speaking.is_empty());
    }

    #[test]
    fn mute_and_deafen() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        // Not connected: toggles are remembered, no op 4.
        assert!(v.toggle_mute().is_empty());
        assert!(v.self_mute && v.control.self_mute());
        v.toggle_mute();
        v.join(Some(G), VC, 0);
        let fx = v.toggle_deafen();
        assert!(matches!(
            fx[0],
            VoiceEffect::Gateway(VoiceRequest {
                self_mute: true,
                self_deaf: true,
                ..
            })
        ));
        assert!(v.control.self_deaf() && v.control.self_mute());
        // Unmuting while deafened undeafens.
        v.toggle_mute();
        assert!(!v.self_deaf && !v.self_mute);
        assert!(!v.prefs.self_deaf && !v.prefs.self_mute);
    }

    #[test]
    fn others_join_and_leave_channels() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        v.apply(
            &Event::VoiceState(Box::new(vs(3, Some(G), Some(VC)))),
            Some(ME),
        );
        v.apply(
            &Event::VoiceState(Box::new(vs(4, Some(G), Some(VC)))),
            Some(ME),
        );
        assert_eq!(v.channel_members(VC).len(), 2);
        v.apply(&Event::VoiceState(Box::new(vs(3, Some(G), None))), Some(ME));
        assert_eq!(v.channel_members(VC).len(), 1);
        v.apply(&Event::GuildDelete(G), Some(ME));
        assert!(v.channel_members(VC).is_empty());
    }

    #[test]
    fn server_disconnect_drops_connection() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        v.join(Some(G), VC, 0);
        v.apply(&server(Some(G), None), Some(ME));
        v.apply(
            &Event::VoiceState(Box::new(vs(1, Some(G), Some(VC)))),
            Some(ME),
        );
        v.apply(&Event::VoiceState(Box::new(vs(1, Some(G), None))), Some(ME));
        assert!(v.conn.is_none());
    }

    #[test]
    fn rejoin_after_expired_session() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        v.join(Some(G), VC, 0);
        v.apply(&server(Some(G), None), Some(ME));
        v.apply(
            &Event::VoiceState(Box::new(vs(1, Some(G), Some(VC)))),
            Some(ME),
        );
        let g1 = v.conn.as_ref().unwrap().generation;
        let fx = v.on_voice_event(
            &VoiceEvent::Ended {
                reason: "x".into(),
                rejoin: true,
            },
            Some(ME),
        );
        assert_eq!(fx.len(), 2);
        assert_eq!(v.conn.as_ref().unwrap().phase, Phase::Requesting);
        v.apply(
            &Event::VoiceState(Box::new(vs(1, Some(G), Some(VC)))),
            Some(ME),
        );
        v.apply(&server(Some(G), None), Some(ME));
        assert!(v.conn.as_ref().unwrap().generation > g1);
        // A fatal end leaves the channel with a toast.
        let fx = v.on_voice_event(
            &VoiceEvent::Ended {
                reason: "kicked".into(),
                rejoin: false,
            },
            Some(ME),
        );
        assert!(fx.iter().any(|e| matches!(e, VoiceEffect::Toast(_))));
        assert!(v.conn.is_none());
    }

    fn call(ringing: &[u64], states: Vec<VoiceState>) -> Event {
        Event::Call(Box::new(CallState {
            channel_id: DM,
            message_id: Some(Snowflake(77)),
            ringing: ringing.iter().map(|u| Snowflake(*u)).collect(),
            voice_states: states,
            unavailable: false,
        }))
    }

    #[test]
    fn incoming_call_ring_accept() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        let fx = v.apply(&call(&[1], vec![vs(2, None, Some(DM))]), Some(ME));
        assert_eq!(fx, vec![VoiceEffect::NotifyIncoming(DM)]);
        assert_eq!(v.incoming, Some(DM));
        assert!(v.active_call(DM));
        // CALL_UPDATE while still ringing does not notify twice.
        assert!(v.apply(&call(&[1], vec![]), Some(ME)).is_empty());
        let fx = v.accept(DM, 0);
        assert!(matches!(
            fx[0],
            VoiceEffect::Gateway(VoiceRequest {
                guild: None,
                channel: Some(DM),
                ..
            })
        ));
        assert_eq!(v.incoming, None);
        // The DM call server update carries the channel id.
        v.apply(&server(None, Some(DM)), Some(ME));
        v.apply(
            &Event::VoiceState(Box::new(vs(1, None, Some(DM)))),
            Some(ME),
        );
        assert_eq!(v.conn.as_ref().unwrap().phase, Phase::Connecting);
        v.apply(&Event::CallDelete(DM), Some(ME));
        assert!(!v.calls.contains_key(&DM));
    }

    #[test]
    fn incoming_call_decline_and_stop() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        v.apply(&call(&[1], vec![vs(2, None, Some(DM))]), Some(ME));
        v.decline(DM);
        assert_eq!(v.incoming, None);
        // Still ringing in the next update: stays declined.
        assert!(v.apply(&call(&[1], vec![]), Some(ME)).is_empty());
        assert_eq!(v.incoming, None);
        // Ringing stopped, then a new ring: shown again.
        v.apply(&call(&[], vec![]), Some(ME));
        assert_eq!(
            v.apply(&call(&[1], vec![]), Some(ME)),
            vec![VoiceEffect::NotifyIncoming(DM)]
        );
        // The caller hung up.
        v.apply(&Event::CallDelete(DM), Some(ME));
        assert_eq!(v.incoming, None);
        // Ringing someone else never shows our overlay.
        assert!(v.apply(&call(&[9], vec![]), Some(ME)).is_empty());
    }

    #[test]
    fn per_user_volume_is_clamped_and_shared() {
        let mut v = Voice::with_prefs(VoicePrefs::default());
        v.set_user(Snowflake(5), 150.0, false);
        assert_eq!(v.control.gain(5), 1.5);
        v.set_user(Snowflake(5), 999.0, true);
        assert_eq!(v.user_volume(Snowflake(5)), (200.0, true));
        assert_eq!(v.control.gain(5), 0.0);
        v.set_user(Snowflake(5), 100.0, false);
        assert!(v.prefs.users.is_empty());
        let json = serde_json::to_string(&v.prefs).unwrap();
        let back: VoicePrefs = serde_json::from_str(&json).unwrap();
        assert_eq!(back, v.prefs);
        let partial: VoicePrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(partial.threshold_db, -60.0);
    }
}
