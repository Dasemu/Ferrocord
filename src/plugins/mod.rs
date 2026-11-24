//! Plugin host: sandboxed WebAssembly plugins (`wasmi`), declared by a
//! manifest, run on one worker thread, reaching the app only through a
//! small, capability-checked host ABI. See `docs/plugins.md`.
//!
//! Zero cost when no plugin is enabled: no engine, no thread, no
//! subscription; the only work at startup is one `stat` of the state file.
//!
//! This module holds the iced-free side (`Plugins`, the registry of what
//! the loaded plugins declared, decorations). The `App` glue lives in
//! `app/plugins.rs`, the views in `ui/plugins.rs`.

pub mod hostcalls;
pub mod kv;
pub mod manifest;
pub mod package;
pub mod runtime;
pub mod state;
pub mod worker;

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

pub use ferrocord_plugin_sdk::abi::{self, Decoration, DecorationKind};
use futures::channel::mpsc;

use hostcalls::{Grant, Out};
use manifest::Manifest;
use package::Scan;
use state::State;

use crate::net::model::{Message, Presence};

/// Messages with decorations kept at most (oldest dropped first).
pub const MAX_DECORATED: usize = 4096;
/// HTTP requests one plugin may have in flight.
pub const MAX_HTTP_INFLIGHT: u8 = 4;

/// Decorations attached by plugins, keyed by message id.
#[derive(Default)]
pub struct Decorations {
    map: HashMap<u64, Vec<(Arc<str>, Decoration)>>,
    order: VecDeque<u64>,
}

impl Decorations {
    /// Replaces `plugin`'s decorations of `message`.
    pub fn set(&mut self, plugin: &str, message: u64, decos: Vec<Decoration>) {
        let entry = self.map.entry(message).or_insert_with(|| {
            self.order.push_back(message);
            Vec::new()
        });
        entry.retain(|(p, _)| &**p != plugin);
        let p: Arc<str> = plugin.into();
        entry.extend(decos.into_iter().map(|d| (p.clone(), d)));
        if entry.is_empty() {
            self.map.remove(&message);
            self.order.retain(|m| *m != message);
        }
        while self.order.len() > MAX_DECORATED {
            if let Some(old) = self.order.pop_front() {
                self.map.remove(&old);
            }
        }
    }

    pub fn remove_plugin(&mut self, plugin: &str) {
        self.map.retain(|_, v| {
            v.retain(|(p, _)| &**p != plugin);
            !v.is_empty()
        });
        let map = &self.map;
        self.order.retain(|m| map.contains_key(m));
    }

    pub fn get(&self, message: u64) -> &[(Arc<str>, Decoration)] {
        self.map.get(&message).map_or(&[], Vec::as_slice)
    }

    pub fn highlighted(&self, message: u64) -> bool {
        self.get(message)
            .iter()
            .any(|(_, d)| d.kind == DecorationKind::Highlight)
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// State of the Plugins settings page.
#[derive(Debug, Default)]
pub struct Page {
    pub open: bool,
    /// Plugin whose permissions are being shown before enabling.
    pub prompt: Option<String>,
    /// Text being typed into string/int settings, by (plugin, key).
    pub drafts: HashMap<(String, String), String>,
    pub scanning: bool,
}

/// A transform waiting for the plugin thread before the send.
#[derive(Debug, Clone)]
pub struct PendingTransform {
    pub ticket: u64,
    pub channel: u64,
    pub content: String,
}

/// The running plugin thread and what its plugins declared.
struct Running {
    handle: worker::Handle,
    generation: u64,
    rx: Arc<Mutex<Option<mpsc::Receiver<Out>>>>,
    /// Sent to the thread, not confirmed yet.
    starting: HashMap<String, Arc<Manifest>>,
    /// Loaded and running.
    loaded: Vec<Arc<Manifest>>,
}

/// The app's plugin state. Holds no engine and no thread until a plugin
/// is enabled.
pub struct Plugins {
    /// `<config dir>/plugins` (`None`: plugins unavailable).
    pub dir: Option<PathBuf>,
    kv_path: Option<PathBuf>,
    /// Write `state.json` (off in mock mode).
    pub persist: bool,
    pub state: State,
    /// Last scan of the plugins directory.
    pub scan: Option<Scan>,
    running: Option<Running>,
    generation: u64,
    pub decorations: Decorations,
    pub page: Page,
    pub pending_transform: Option<PendingTransform>,
    next_ticket: u64,
    next_grant: u64,
    pub http_inflight: HashMap<String, u8>,
    pub http: Option<reqwest::Client>,
}

impl Plugins {
    pub fn new(dir: Option<PathBuf>, kv_path: Option<PathBuf>, persist: bool) -> Self {
        Self {
            dir,
            kv_path,
            persist,
            state: State::default(),
            scan: None,
            running: None,
            generation: 0,
            decorations: Decorations::default(),
            page: Page::default(),
            pending_transform: None,
            next_ticket: 0,
            next_grant: 0,
            http_inflight: HashMap::new(),
            http: None,
        }
    }

    /// Whether startup has anything to look at: the state file exists.
    /// One `stat`, nothing else, when the user never enabled a plugin.
    pub fn may_have_enabled(&self) -> bool {
        self.dir
            .as_ref()
            .is_some_and(|d| d.join(state::STATE_FILE).is_file())
    }

    /// The plugin thread and wasm engine exist.
    pub fn is_running(&self) -> bool {
        self.running.is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The receiver of the running thread's outputs (taken once by the
    /// subscription).
    pub fn receiver(&self) -> Option<Arc<Mutex<Option<mpsc::Receiver<Out>>>>> {
        self.running.as_ref().map(|r| r.rx.clone())
    }

    /// Starts the thread if needed.
    fn ensure_running(&mut self) -> Option<&mut Running> {
        if self.running.is_none() {
            let (tx, rx) = mpsc::channel(worker::OUT_QUEUE);
            match worker::spawn(self.kv_path.clone(), tx, crate::app::now_ms) {
                Ok(handle) => {
                    self.generation += 1;
                    self.running = Some(Running {
                        handle,
                        generation: self.generation,
                        rx: Arc::new(Mutex::new(Some(rx))),
                        starting: HashMap::new(),
                        loaded: Vec::new(),
                    });
                }
                Err(e) => {
                    tracing::warn!(error = %e, "could not start the plugin thread");
                    return None;
                }
            }
        }
        self.running.as_mut()
    }

    /// Loads `m` from `source` (enabling must already be recorded).
    pub fn load(&mut self, m: &Manifest, source: worker::Source) {
        let settings = self
            .state
            .plugins
            .get(&m.id)
            .map(|r| r.settings.clone())
            .unwrap_or_default();
        let Some(r) = self.ensure_running() else {
            return;
        };
        r.starting.insert(m.id.clone(), Arc::new(m.clone()));
        r.handle.send(worker::Job::Load(Box::new(worker::LoadSpec {
            manifest: m.clone(),
            source,
            settings,
        })));
    }

    /// Unloads `id`; stops the thread when nothing is left.
    pub fn unload(&mut self, id: &str) {
        self.decorations.remove_plugin(id);
        self.http_inflight.remove(id);
        let Some(r) = self.running.as_mut() else {
            return;
        };
        r.handle.send(worker::Job::Unload(id.to_owned()));
        r.loaded.retain(|m| m.id != id);
        r.starting.remove(id);
        self.stop_if_idle();
    }

    /// Drops the thread (and engine) when no plugin is loaded or starting.
    fn stop_if_idle(&mut self) {
        if self
            .running
            .as_ref()
            .is_some_and(|r| r.loaded.is_empty() && r.starting.is_empty())
        {
            // A pending transform is kept: its timeout sends the text as
            // typed, so stopping the thread never loses a message.
            self.running = None;
        }
    }

    /// The thread confirmed (or refused) a load. Returns the manifest.
    pub fn loaded(&mut self, id: &str, ok: bool) -> Option<Arc<Manifest>> {
        let r = self.running.as_mut()?;
        let m = r.starting.remove(id)?;
        if ok {
            r.loaded.retain(|o| o.id != id);
            r.loaded.push(m.clone());
        } else {
            self.stop_if_idle();
        }
        Some(m)
    }

    /// A fault disabled `id` on the thread side.
    pub fn faulted_out(&mut self, id: &str) {
        self.decorations.remove_plugin(id);
        if let Some(r) = self.running.as_mut() {
            r.loaded.retain(|m| m.id != id);
            r.starting.remove(id);
        }
        self.stop_if_idle();
    }

    /// Running plugins (loaded).
    pub fn active(&self) -> &[Arc<Manifest>] {
        self.running.as_ref().map_or(&[], |r| r.loaded.as_slice())
    }

    pub fn manifest(&self, id: &str) -> Option<&Arc<Manifest>> {
        self.active().iter().find(|m| m.id == id)
    }

    /// Whether any running plugin subscribed to event `name`.
    pub fn wants(&self, name: &str) -> bool {
        self.active()
            .iter()
            .any(|m| m.events.iter().any(|e| e == name))
    }

    pub fn has_transform(&self) -> bool {
        self.active()
            .iter()
            .any(|m| m.has(manifest::Permission::TransformOutgoing))
    }

    /// The plugin that registered `/name` (first enabled wins).
    pub fn command(&self, name: &str) -> Option<&Arc<Manifest>> {
        self.active()
            .iter()
            .find(|m| m.commands.iter().any(|c| c.name == name))
    }

    /// Context-menu actions: (plugin id, action id, label).
    pub fn actions(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.active().iter().flat_map(|m| {
            m.actions
                .iter()
                .map(move |a| (m.id.as_str(), a.id.as_str(), a.label.as_str()))
        })
    }

    /// Queues an observer event if any plugin listens to it.
    pub fn observe(&self, ev: abi::Event) {
        if let Some(r) = &self.running
            && self.wants(ev.name())
        {
            r.handle.event(ev);
        }
    }

    pub fn send(&self, job: worker::Job) {
        if let Some(r) = &self.running {
            r.handle.send(job);
        }
    }

    /// A grant for one user action in `channel` (and on `message`).
    pub fn grant(&mut self, channel: u64, guild: Option<u64>, message: Option<u64>) -> Grant {
        self.next_grant += 1;
        Grant {
            id: self.next_grant,
            channel,
            guild,
            message,
            expires_ms: crate::app::now_ms() + hostcalls::GRANT_TTL_MS,
        }
    }

    pub fn next_ticket(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }

    /// Observer events dropped because plugins were too slow.
    pub fn dropped_events(&self) -> u64 {
        self.running.as_ref().map_or(0, |r| r.handle.dropped())
    }

    pub fn running_generation(&self) -> Option<u64> {
        self.running.as_ref().map(|r| r.generation)
    }
}

/// A chat message as plugins see it.
pub fn message_info(m: &Message, me: Option<crate::net::model::UserId>) -> abi::MessageInfo {
    abi::MessageInfo {
        id: m.id.0,
        channel_id: m.channel_id.0,
        guild_id: m.guild_id.map(|g| g.0),
        author_id: m.author.id.0,
        author_name: m.author.display_name().to_owned(),
        content: m.content.clone(),
        timestamp_ms: m.timestamp_ms(),
        mine: Some(m.author.id) == me,
    }
}

pub fn presence_name(p: Presence) -> &'static str {
    match p {
        Presence::Online => "online",
        Presence::Idle => "idle",
        Presence::Dnd => "dnd",
        Presence::Offline => "offline",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn deco(kind: DecorationKind, t: &str) -> Decoration {
        Decoration {
            kind,
            text: t.into(),
            tooltip: None,
        }
    }

    #[test]
    fn decorations_replace_per_plugin_and_stay_bounded() {
        let mut d = Decorations::default();
        d.set("a", 1, vec![deco(DecorationKind::Badge, "x")]);
        d.set("b", 1, vec![deco(DecorationKind::Highlight, "")]);
        assert_eq!(d.get(1).len(), 2);
        assert!(d.highlighted(1));
        d.set("b", 1, vec![]);
        assert!(!d.highlighted(1));
        assert_eq!(d.get(1).len(), 1);
        d.remove_plugin("a");
        assert!(d.is_empty());
        for i in 0..(MAX_DECORATED as u64 + 10) {
            d.set("a", i, vec![deco(DecorationKind::Line, "l")]);
        }
        assert_eq!(d.len(), MAX_DECORATED);
        assert!(d.get(0).is_empty());
    }

    #[test]
    fn nothing_runs_without_plugins() {
        let dir = tempfile::tempdir().unwrap();
        let p = Plugins::new(Some(dir.path().to_path_buf()), None, false);
        assert!(!p.may_have_enabled());
        assert!(!p.is_running());
        assert!(p.receiver().is_none());
        assert!(!p.wants("message_create"));
    }
}
