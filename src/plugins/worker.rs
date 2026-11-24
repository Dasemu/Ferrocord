//! The plugin thread. All plugin code runs here, never on the UI thread:
//! one thread for all plugins, a priority queue for user actions and a
//! bounded, coalescing queue for observer events (dropped when plugins
//! are slow, so the app never waits and memory stays bounded).
//!
//! The thread (and the wasm engine) only exist while at least one plugin
//! is enabled.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ferrocord_plugin_sdk::abi::{
    self, ActionInput, CommandInput, Event, HttpResponse, LoadInfo, SettingChanged, SettingValue,
    TransformInput, TransformOutput, hook,
};
use futures::channel::mpsc;

use super::hostcalls::{CallKind, Env, Grant, Out};
use super::kv::Kv;
use super::manifest::{Manifest, Permission};
use super::package;
use super::runtime::{self, Fault, Instance};

/// Observer events waiting at most; older ones are dropped beyond this.
pub const EVENT_QUEUE: usize = 256;
/// Faults (traps, fuel exhaustion, panics) before a plugin is disabled.
pub const MAX_FAULTS: u32 = 3;
/// Wall-clock budget of a whole transform chain.
const TRANSFORM_BUDGET: Duration = Duration::from_millis(150);
/// Capacity of the plugin → app channel.
pub const OUT_QUEUE: usize = 1024;

/// Where the module comes from.
pub enum Source {
    Path(PathBuf),
    /// In-memory module (tests).
    Bytes(Vec<u8>),
}

pub struct LoadSpec {
    pub manifest: Manifest,
    pub source: Source,
    pub settings: BTreeMap<String, SettingValue>,
}

pub enum Job {
    Load(Box<LoadSpec>),
    Unload(String),
    Transform {
        ticket: u64,
        input: TransformInput,
    },
    Command {
        id: String,
        input: CommandInput,
        grant: Grant,
    },
    Action {
        id: String,
        input: ActionInput,
        grant: Grant,
    },
    SettingChanged {
        id: String,
        key: String,
        value: SettingValue,
    },
    HttpDone {
        id: String,
        response: HttpResponse,
        grant: Option<Grant>,
    },
}

#[derive(Default)]
struct Queues {
    prio: VecDeque<Job>,
    events: VecDeque<Event>,
    dropped: u64,
    closed: bool,
}

type Shared = Arc<(Mutex<Queues>, Condvar)>;

/// The app's handle to the plugin thread. Dropping the last clone stops it.
#[derive(Clone)]
pub struct Handle {
    shared: Shared,
    _stop: Arc<StopOnDrop>,
}

struct StopOnDrop(Shared);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Ok(mut q) = self.0.0.lock() {
            q.closed = true;
        }
        self.0.1.notify_all();
    }
}

impl Handle {
    /// Queues a job (user actions and control jobs are never dropped).
    pub fn send(&self, job: Job) {
        if let Ok(mut q) = self.shared.0.lock() {
            q.prio.push_back(job);
        }
        self.shared.1.notify_one();
    }

    /// Queues an observer event, coalescing repeats and dropping the
    /// oldest when the queue is full.
    pub fn event(&self, ev: Event) {
        if let Ok(mut q) = self.shared.0.lock() {
            push_event(&mut q, ev);
        }
        self.shared.1.notify_one();
    }

    /// Observer events dropped so far.
    pub fn dropped(&self) -> u64 {
        self.shared.0.lock().map(|q| q.dropped).unwrap_or(0)
    }
}

/// A newer channel switch replaces a queued one, a newer history of a
/// channel replaces its queued history; beyond [`EVENT_QUEUE`] the oldest
/// event is dropped.
fn push_event(q: &mut Queues, ev: Event) {
    let replaced = match &ev {
        Event::ChannelSwitch { .. } => q
            .events
            .iter_mut()
            .rev()
            .find(|e| matches!(e, Event::ChannelSwitch { .. })),
        Event::History { channel_id, .. } => q
            .events
            .iter_mut()
            .find(|e| matches!(e, Event::History { channel_id: c, .. } if c == channel_id)),
        _ => None,
    };
    match replaced {
        Some(slot) => *slot = ev,
        None => {
            if q.events.len() >= EVENT_QUEUE {
                q.events.pop_front();
                q.dropped += 1;
            }
            q.events.push_back(ev);
        }
    }
}

enum Next {
    Job(Job),
    Event(Event),
}

fn next(shared: &Shared) -> Option<Next> {
    let (lock, cv) = &**shared;
    let mut q = lock.lock().ok()?;
    loop {
        if q.closed {
            return None;
        }
        if let Some(j) = q.prio.pop_front() {
            return Some(Next::Job(j));
        }
        if let Some(e) = q.events.pop_front() {
            return Some(Next::Event(e));
        }
        q = cv.wait(q).ok()?;
    }
}

/// Starts the plugin thread. `kv_path`: the plugin store (`None`: in
/// memory). Outputs go to `out`.
pub fn spawn(
    kv_path: Option<PathBuf>,
    out: mpsc::Sender<Out>,
    now_ms: fn() -> u64,
) -> std::io::Result<Handle> {
    let shared: Shared = Arc::new((Mutex::new(Queues::default()), Condvar::new()));
    let s2 = shared.clone();
    std::thread::Builder::new()
        .name("ferrocord-plugins".into())
        .spawn(move || {
            let mut w = Worker::new(kv_path, out, now_ms);
            while let Some(n) = next(&s2) {
                match n {
                    Next::Job(j) => w.job(j),
                    Next::Event(e) => w.event(&e),
                }
                if w.closed {
                    break;
                }
            }
            w.unload_all();
            tracing::debug!("plugin thread stopped");
        })?;
    Ok(Handle {
        _stop: Arc::new(StopOnDrop(shared.clone())),
        shared,
    })
}

struct Plugin {
    manifest: Arc<Manifest>,
    module: wasmi::Module,
    inst: Option<Instance>,
    faults: u32,
}

pub struct Worker {
    engine: Option<wasmi::Engine>,
    plugins: Vec<Plugin>,
    kv: Rc<RefCell<Kv>>,
    out: mpsc::Sender<Out>,
    now_ms: fn() -> u64,
    /// The app side is gone.
    closed: bool,
}

impl Worker {
    pub fn new(kv_path: Option<PathBuf>, out: mpsc::Sender<Out>, now_ms: fn() -> u64) -> Self {
        Self {
            engine: None,
            plugins: Vec::new(),
            kv: Rc::new(RefCell::new(Kv::new(kv_path))),
            out,
            now_ms,
            closed: false,
        }
    }

    fn emit(&mut self, o: Out) {
        if let Err(e) = self.out.try_send(o) {
            if e.is_disconnected() {
                self.closed = true;
            } else {
                tracing::warn!("plugin output queue full; dropping a request");
            }
        }
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.plugins.iter().position(|p| p.manifest.id == id)
    }

    pub fn job(&mut self, j: Job) {
        match j {
            Job::Load(spec) => self.load(*spec),
            Job::Unload(id) => {
                if let Some(i) = self.index(&id) {
                    self.unload(i);
                }
            }
            Job::Transform { ticket, input } => {
                let text = self.transform(input);
                self.emit(Out::Transformed { ticket, text });
            }
            Job::Command { id, input, grant } => {
                if let (Some(i), Ok(p)) = (self.index(&id), abi::encode(&input)) {
                    self.run(
                        i,
                        hook::COMMAND,
                        &p,
                        CallKind::User(grant),
                        runtime::ACTION_FUEL,
                    );
                }
            }
            Job::Action { id, input, grant } => {
                if let (Some(i), Ok(p)) = (self.index(&id), abi::encode(&input)) {
                    self.run(
                        i,
                        hook::ACTION,
                        &p,
                        CallKind::User(grant),
                        runtime::ACTION_FUEL,
                    );
                }
            }
            Job::SettingChanged { id, key, value } => {
                let Some(i) = self.index(&id) else { return };
                if let Some(inst) = self.plugins[i].inst.as_mut() {
                    inst.env_mut().settings.insert(key.clone(), value.clone());
                }
                if let Ok(p) = abi::encode(&SettingChanged { key, value }) {
                    self.run(
                        i,
                        hook::SETTING_CHANGED,
                        &p,
                        CallKind::Passive,
                        runtime::EVENT_FUEL,
                    );
                }
            }
            Job::HttpDone {
                id,
                response,
                grant,
            } => {
                let kind = grant.map_or(CallKind::Passive, CallKind::User);
                if let (Some(i), Ok(p)) = (self.index(&id), abi::encode(&response)) {
                    self.run(i, hook::HTTP_RESPONSE, &p, kind, runtime::ACTION_FUEL);
                }
            }
        }
    }

    pub fn event(&mut self, ev: &Event) {
        let name = ev.name();
        let Ok(payload) = abi::encode(ev) else {
            return;
        };
        for i in 0..self.plugins.len() {
            let p = &self.plugins[i];
            if p.inst.is_none() || !p.manifest.events.iter().any(|e| e == name) {
                continue;
            }
            self.run(
                i,
                hook::EVENT,
                &payload,
                CallKind::Passive,
                runtime::EVENT_FUEL,
            );
        }
    }

    /// Chains every transforming plugin over `input`. `None`: unchanged.
    fn transform(&mut self, mut input: TransformInput) -> Option<String> {
        let started = Instant::now();
        let original = input.text.clone();
        for i in 0..self.plugins.len() {
            if self.plugins[i].inst.is_none()
                || !self.plugins[i].manifest.has(Permission::TransformOutgoing)
            {
                continue;
            }
            if started.elapsed() > TRANSFORM_BUDGET {
                tracing::warn!("plugin transforms took too long; skipping the rest");
                break;
            }
            let Ok(p) = abi::encode(&input) else { break };
            let Some(Some(out)) = self.run(
                i,
                hook::TRANSFORM,
                &p,
                CallKind::Transform,
                runtime::TRANSFORM_FUEL,
            ) else {
                continue;
            };
            if let Ok(TransformOutput { text: Some(t) }) = abi::decode::<TransformOutput>(&out)
                && !t.trim().is_empty()
                && t.chars().count() <= 2000
            {
                input.text = t;
            }
        }
        (input.text != original).then_some(input.text)
    }

    /// Runs one hook of plugin `i`, isolating every failure. `None`: it
    /// faulted.
    fn run(
        &mut self,
        i: usize,
        hook_id: i32,
        payload: &[u8],
        kind: CallKind,
        fuel: u64,
    ) -> Option<Option<Vec<u8>>> {
        let inst = self.plugins.get_mut(i)?.inst.as_mut()?;
        let r = catch_unwind(AssertUnwindSafe(|| inst.call(hook_id, payload, kind, fuel)))
            .unwrap_or(Err(Fault::Panic));
        match r {
            Ok(o) => {
                let outs = std::mem::take(&mut inst.env_mut().out);
                for x in outs {
                    self.emit(x);
                }
                Some(o)
            }
            Err(f) => {
                self.fault(i, f);
                None
            }
        }
    }

    fn fault(&mut self, i: usize, f: Fault) {
        let Some(p) = self.plugins.get_mut(i) else {
            return;
        };
        p.faults += 1;
        let id = p.manifest.id.clone();
        tracing::warn!(plugin = %id, fault = %f, faults = p.faults, "plugin fault");
        let mut disabled = p.faults >= MAX_FAULTS;
        if !disabled {
            // The instance may be corrupt (a panic mid-allocation):
            // start it again from scratch, keeping its settings.
            let old = p.inst.take().map(Instance::into_env);
            match self.instantiate(i, old) {
                Ok(()) => {}
                Err(e) => {
                    tracing::warn!(plugin = %id, error = %e, "plugin restart failed");
                    disabled = true;
                }
            }
        }
        if disabled {
            self.plugins.remove(i);
        }
        self.emit(Out::Faulted {
            id,
            reason: f.to_string(),
            disabled,
        });
    }

    /// (Re)creates plugin `i`'s instance and runs its `load` hook.
    fn instantiate(&mut self, i: usize, old: Option<Env>) -> Result<(), String> {
        let engine = self.engine.get_or_insert_with(runtime::engine).clone();
        let p = self.plugins.get_mut(i).ok_or("gone")?;
        let settings = old.as_ref().map(|e| e.settings.clone()).unwrap_or_default();
        let mut env = Env::new(
            p.manifest.clone(),
            settings,
            self.kv.clone(),
            runtime::limits(),
            self.now_ms,
        );
        if let Some(old) = old {
            env.carry_over(old);
        }
        let module = p.module.clone();
        let mut inst = catch_unwind(AssertUnwindSafe(|| Instance::new(&engine, &module, env)))
            .unwrap_or(Err(Fault::Panic))
            .map_err(|f| f.to_string())?;
        let info = LoadInfo {
            api_version: abi::ABI_VERSION,
            plugin_id: p.manifest.id.clone(),
            settings: inst.env().settings.clone(),
        };
        let payload = abi::encode(&info).map_err(|e| e.to_string())?;
        catch_unwind(AssertUnwindSafe(|| {
            inst.call(hook::LOAD, &payload, CallKind::Passive, runtime::LOAD_FUEL)
        }))
        .unwrap_or(Err(Fault::Panic))
        .map_err(|f| f.to_string())?;
        let outs = std::mem::take(&mut inst.env_mut().out);
        p.inst = Some(inst);
        for x in outs {
            self.emit(x);
        }
        Ok(())
    }

    fn load(&mut self, spec: LoadSpec) {
        let id = spec.manifest.id.clone();
        if let Some(i) = self.index(&id) {
            self.unload(i);
        }
        let result = (|| {
            let wasm = match spec.source {
                Source::Path(p) => package::read_wasm(&p)?,
                Source::Bytes(b) => b,
            };
            let engine = self.engine.get_or_insert_with(runtime::engine).clone();
            let module = runtime::compile(&engine, &wasm)?;
            let manifest = Arc::new(spec.manifest);
            let settings = manifest.effective_settings(&spec.settings);
            self.plugins.push(Plugin {
                manifest: manifest.clone(),
                module,
                inst: None,
                faults: 0,
            });
            let i = self.plugins.len() - 1;
            let env = Env::new(
                manifest,
                settings,
                self.kv.clone(),
                runtime::limits(),
                self.now_ms,
            );
            let r = self.instantiate(i, Some(env));
            if r.is_err() {
                self.plugins.pop();
            }
            r
        })();
        if let Err(e) = &result {
            tracing::warn!(plugin = %id, error = %e, "plugin failed to load");
        }
        self.emit(Out::Loaded {
            id,
            error: result.err(),
        });
    }

    fn unload(&mut self, i: usize) {
        let Some(id) = self.plugins.get(i).map(|p| p.manifest.id.clone()) else {
            return;
        };
        self.run(i, hook::UNLOAD, &[], CallKind::Passive, runtime::EVENT_FUEL);
        // A fault in `unload` may already have removed it.
        if let Some(j) = self.index(&id) {
            self.plugins.remove(j);
        }
    }

    fn unload_all(&mut self) {
        while !self.plugins.is_empty() {
            self.unload(self.plugins.len() - 1);
        }
    }

    /// Loaded plugin ids (tests).
    pub fn loaded(&self) -> Vec<String> {
        self.plugins.iter().map(|p| p.manifest.id.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn del(id: u64) -> Event {
        Event::MessageDelete { id, channel_id: 1 }
    }

    #[test]
    fn event_queue_is_bounded_and_coalesces() {
        let mut q = Queues::default();
        for i in 0..(EVENT_QUEUE as u64 + 10) {
            push_event(&mut q, del(i));
        }
        assert_eq!(q.events.len(), EVENT_QUEUE);
        assert_eq!(q.dropped, 10);
        assert_eq!(q.events.front(), Some(&del(10)));

        let mut q = Queues::default();
        for c in [Some(1), Some(2)] {
            push_event(
                &mut q,
                Event::ChannelSwitch {
                    channel_id: c,
                    guild_id: None,
                },
            );
        }
        for n in [1, 2] {
            push_event(
                &mut q,
                Event::History {
                    channel_id: 7,
                    messages: vec![Default::default(); n],
                },
            );
        }
        assert_eq!(q.events.len(), 2);
        assert!(matches!(
            q.events[0],
            Event::ChannelSwitch {
                channel_id: Some(2),
                ..
            }
        ));
        assert!(matches!(&q.events[1], Event::History { messages, .. } if messages.len() == 2));
    }

    #[test]
    fn user_actions_go_before_events() {
        let shared: Shared = Arc::new((Mutex::new(Queues::default()), Condvar::new()));
        if let Ok(mut q) = shared.0.lock() {
            push_event(&mut q, del(1));
            q.prio.push_back(Job::Unload("x".into()));
        }
        assert!(matches!(next(&shared), Some(Next::Job(Job::Unload(_)))));
        assert!(matches!(next(&shared), Some(Next::Event(_))));
        if let Ok(mut q) = shared.0.lock() {
            q.closed = true;
        }
        assert!(next(&shared).is_none());
    }
}
