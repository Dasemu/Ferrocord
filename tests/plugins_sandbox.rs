//! Sandbox tests with hostile plugins written in WebAssembly text: no host
//! call without its permission, no network outside the allowlist, no
//! `send_message` outside a user action, fuel and memory limits, traps and
//! panics isolated, auto-disable after repeated faults.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ferrocord::app::plugins::PluginMsg;
use ferrocord::app::{App, Message};
use ferrocord::config::Config;
use ferrocord::plugins::abi::{self, HttpRequest, KeyReq, TextReq, err, hook, op};
use ferrocord::plugins::hostcalls::{CallKind, Env, Grant, Out};
use ferrocord::plugins::kv::Kv;
use ferrocord::plugins::manifest::Manifest;
use ferrocord::plugins::runtime::{self, Fault, Instance};
use ferrocord::plugins::worker::{Job, LoadSpec, MAX_FAULTS, Source, Worker};
use futures::channel::mpsc;

fn manifest(perms: &str, hosts: &str) -> Manifest {
    Manifest::parse(&format!(
        "id = \"evil\"\nname = \"Evil\"\nversion = \"1\"\napi_version = 1\n\
         permissions = [{perms}]\nnetwork_hosts = [{hosts}]\nevents = [\"channel_switch\"]\n"
    ))
    .unwrap()
}

fn escape(b: &[u8]) -> String {
    b.iter().map(|x| format!("\\{x:02x}")).collect()
}

/// A module with the ABI exports. `$data` holds `data` at offset 16 and
/// `fc_call`'s body is `body` (params `$h $p $l`, result i64).
fn module(body: &str, data: &[u8], pages: u32) -> Vec<u8> {
    let src = format!(
        r#"(module
  (import "ferrocord" "host_call" (func $hc (param i32 i32 i32) (result i32)))
  (import "ferrocord" "host_take" (func $ht (param i32 i32) (result i32)))
  (memory (export "memory") {pages})
  (global $bump (mut i32) (i32.const 4096))
  (data (i32.const 16) "{data}")
  (func (export "fc_abi_version") (result i32) (i32.const 1))
  (func (export "fc_alloc") (param $n i32) (result i32) (local $p i32)
    (local.set $p (global.get $bump))
    (global.set $bump (i32.add (global.get $bump) (local.get $n)))
    (local.get $p))
  (func $rec (param i32) (result i32) (call $rec (i32.add (local.get 0) (i32.const 1))))
  (func (export "fc_call") (param $h i32) (param $p i32) (param $l i32) (result i64)
    (global.set $bump (i32.const 4096))
    {body}))"#,
        data = escape(data),
    );
    wat::parse_str(src).unwrap()
}

/// `fc_call` makes host call `op` with the data and returns its result
/// code as a 4-byte output at offset 0.
fn call_and_report(op_id: i32, data: &[u8]) -> Vec<u8> {
    module(
        &format!(
            "(i32.store (i32.const 0) (call $hc (i32.const {op_id}) (i32.const 16) (i32.const {len})))
             (i64.const 4)",
            len = data.len()
        ),
        data,
        1,
    )
}

fn env(m: Manifest) -> Env {
    let settings = m.effective_settings(&BTreeMap::new());
    Env::new(
        Arc::new(m),
        settings,
        Rc::new(RefCell::new(Kv::new(None))),
        runtime::limits(),
        || 1_000,
    )
}

fn instance(wasm: &[u8], m: Manifest) -> Result<Instance, Fault> {
    let engine = runtime::engine();
    let module = runtime::compile(&engine, wasm).unwrap();
    Instance::new(&engine, &module, env(m))
}

fn grant() -> CallKind {
    CallKind::User(Grant {
        id: 1,
        channel: 5,
        guild: None,
        message: Some(9),
        expires_ms: 60_000,
    })
}

fn code(i: &mut Instance, kind: CallKind) -> i32 {
    let out = i
        .call(hook::EVENT, &[], kind, runtime::EVENT_FUEL)
        .unwrap()
        .unwrap();
    i32::from_le_bytes(out[..4].try_into().unwrap())
}

fn text(t: &str) -> Vec<u8> {
    abi::encode(&TextReq { text: t.into() }).unwrap()
}

#[test]
fn host_calls_without_permission_are_refused() {
    let key = abi::encode(&KeyReq { key: "k".into() }).unwrap();
    for (o, data) in [
        (op::SEND_MESSAGE, text("hi")),
        (op::KV_GET, key.clone()),
        (op::KV_SET, key),
        (op::CLIPBOARD_WRITE, text("x")),
        (op::COMPOSER_INSERT, text("x")),
        (op::ADD_REACTION, text("👍")),
    ] {
        let mut i = instance(&call_and_report(o, &data), manifest("", "")).unwrap();
        assert_eq!(code(&mut i, grant()), err::DENIED, "op {o}");
        assert!(i.env().out.is_empty());
    }
}

#[test]
fn network_is_limited_to_the_allowlist() {
    let req = |url: &str| {
        abi::encode(&HttpRequest {
            method: "GET".into(),
            url: url.into(),
            ..Default::default()
        })
        .unwrap()
    };
    let m = || manifest("\"network\"", "\"api.example.com\"");
    for bad in [
        "https://discord.com/api/v9/users/@me",
        "https://evil.example.net/",
        "http://api.example.com/",
    ] {
        let mut i = instance(&call_and_report(op::HTTP_FETCH, &req(bad)), m()).unwrap();
        assert_eq!(
            code(&mut i, CallKind::Passive),
            err::HOST_NOT_ALLOWED,
            "{bad}"
        );
        assert!(i.env().out.is_empty());
    }
    // Without the permission even an allowlisted URL is refused.
    let mut i = instance(
        &call_and_report(op::HTTP_FETCH, &req("https://api.example.com/")),
        manifest("", ""),
    )
    .unwrap();
    assert_eq!(code(&mut i, CallKind::Passive), err::DENIED);
    // Allowed: returns an id (MessagePack positive fixint 1 = 1 byte).
    let mut i = instance(
        &call_and_report(op::HTTP_FETCH, &req("https://api.example.com/x")),
        m(),
    )
    .unwrap();
    assert_eq!(code(&mut i, CallKind::Passive), 1);
    assert!(matches!(
        i.env().out.as_slice(),
        [Out::Http { request: 1, .. }]
    ));
}

#[test]
fn send_message_only_in_a_user_action() {
    let wasm = call_and_report(op::SEND_MESSAGE, &text("hello"));
    let mut i = instance(&wasm, manifest("\"send_messages\"", "")).unwrap();
    assert_eq!(code(&mut i, CallKind::Passive), err::NOT_USER_ACTION);
    assert_eq!(code(&mut i, CallKind::Transform), err::DENIED);
    assert!(i.env().out.is_empty());
    assert_eq!(code(&mut i, grant()), 0);
    assert_eq!(
        i.env().out,
        [Out::Send {
            id: "evil".into(),
            channel: 5,
            content: "hello".into()
        }]
    );
    // One message per user action.
    assert_eq!(code(&mut i, grant()), err::LIMIT);
}

#[test]
fn infinite_loops_run_out_of_fuel() {
    let wasm = module("(loop $l (br $l)) (i64.const 0)", &[], 1);
    let mut i = instance(&wasm, manifest("", "")).unwrap();
    let t = Instant::now();
    let r = i.call(hook::EVENT, &[], CallKind::Passive, runtime::EVENT_FUEL);
    assert_eq!(r, Err(Fault::OutOfFuel));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    // A loop at instantiation (start function) is bounded too.
    let src = r#"(module (memory (export "memory") 1)
        (func $s (loop $l (br $l))) (start $s))"#;
    let wasm = wat::parse_str(src).unwrap();
    assert!(matches!(
        instance(&wasm, manifest("", "")),
        Err(Fault::OutOfFuel)
    ));
}

#[test]
fn traps_and_stack_overflow_are_faults() {
    let mut i = instance(&module("unreachable", &[], 1), manifest("", "")).unwrap();
    assert!(matches!(
        i.call(hook::EVENT, &[], CallKind::Passive, runtime::EVENT_FUEL),
        Err(Fault::Trap(_))
    ));
    let mut i = instance(
        &module("(drop (call $rec (i32.const 0))) (i64.const 0)", &[], 1),
        manifest("", ""),
    )
    .unwrap();
    assert!(matches!(
        i.call(hook::EVENT, &[], CallKind::Passive, runtime::EVENT_FUEL),
        Err(Fault::Trap(_))
    ));
}

#[test]
fn memory_is_capped() {
    // memory.grow past 16 MiB fails (returns -1) instead of allocating.
    let wasm = module(
        "(if (i32.eq (memory.grow (i32.const 400)) (i32.const -1))
            (then (return (i64.const 0))))
         unreachable",
        &[],
        1,
    );
    let mut i = instance(&wasm, manifest("", "")).unwrap();
    assert_eq!(
        i.call(hook::EVENT, &[], CallKind::Passive, runtime::EVENT_FUEL),
        Ok(None)
    );
    // A module that starts above the cap does not instantiate.
    assert!(instance(&module("(i64.const 0)", &[], 400), manifest("", "")).is_err());
}

#[test]
fn bad_pointers_never_reach_the_host() {
    // Out-of-bounds input to a host call: refused, no panic.
    let wasm = module(
        "(i32.store (i32.const 0) (call $hc (i32.const 2) (i32.const -16) (i32.const 64)))
         (i64.const 4)",
        &[],
        1,
    );
    let mut i = instance(&wasm, manifest("", "")).unwrap();
    assert_eq!(code(&mut i, CallKind::Passive), err::BAD_INPUT);
    // A bogus output pointer is an ABI fault, not a host crash.
    let wasm = module("(i64.const 0x7fffffff00000010)", &[], 1);
    let mut i = instance(&wasm, manifest("", "")).unwrap();
    assert!(matches!(
        i.call(hook::EVENT, &[], CallKind::Passive, runtime::EVENT_FUEL),
        Err(Fault::Abi(_))
    ));
    // Missing exports.
    let wasm = wat::parse_str(r#"(module (memory (export "memory") 1))"#).unwrap();
    assert!(matches!(
        instance(&wasm, manifest("", "")),
        Err(Fault::Abi(_))
    ));
}

fn drain(rx: &mut mpsc::Receiver<Out>) -> Vec<Out> {
    let mut v = Vec::new();
    while let Ok(o) = rx.try_recv() {
        v.push(o);
    }
    v
}

/// The plugin toasts, then traps on every event: the toast of a faulting
/// call is dropped, the instance restarts, and after `MAX_FAULTS` the
/// plugin is disabled.
#[test]
fn repeated_faults_disable_the_plugin_and_the_app_keeps_running() {
    let wasm = module(
        &format!(
            "(if (i32.eq (local.get $h) (i32.const {})) (then (return (i64.const 0))))
             (drop (call $hc (i32.const {}) (i32.const 16) (i32.const {})))
             unreachable",
            hook::LOAD,
            op::TOAST,
            text("boom").len()
        ),
        &text("boom"),
        1,
    );
    let (tx, mut rx) = mpsc::channel(64);
    let mut w = Worker::new(None, tx, || 1_000);
    w.job(Job::Load(Box::new(LoadSpec {
        manifest: manifest("", ""),
        source: Source::Bytes(wasm),
        settings: BTreeMap::new(),
    })));
    assert_eq!(
        drain(&mut rx),
        [Out::Loaded {
            id: "evil".into(),
            error: None
        }]
    );
    let ev = abi::Event::ChannelSwitch {
        channel_id: None,
        guild_id: None,
    };
    let mut outs = Vec::new();
    for _ in 0..MAX_FAULTS {
        w.event(&ev);
        outs.extend(drain(&mut rx));
    }
    assert!(!outs.iter().any(|o| matches!(o, Out::Toast { .. })));
    let faults: Vec<bool> = outs
        .iter()
        .filter_map(|o| match o {
            Out::Faulted { disabled, .. } => Some(*disabled),
            _ => None,
        })
        .collect();
    assert_eq!(faults, [false, false, true]);
    assert!(w.loaded().is_empty());

    // The app side: a toast, the plugin is disabled and stays disabled.
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_tests();
    cfg.plugins_dir = Some(dir.path().to_path_buf());
    let mut a = App::boot(cfg).0;
    let m = manifest("", "");
    a.plugins.state.enable(&m);
    let _ = a.update(Message::Plugin(PluginMsg::Out(Box::new(
        outs.last().cloned().unwrap(),
    ))));
    assert!(!a.plugins.state.is_enabled("evil"));
    assert!(
        a.toast
            .as_deref()
            .unwrap_or_default()
            .contains("turned off")
    );
    assert!(
        a.plugins.state.plugins["evil"]
            .disabled_reason
            .as_deref()
            .is_some()
    );
    // Still responsive.
    let _ = a.update(Message::Ui(ferrocord::app::Ui::Tick));
}

#[test]
fn wrong_abi_version_is_refused() {
    let src = r#"(module (memory (export "memory") 1)
        (func (export "fc_abi_version") (result i32) (i32.const 2))
        (func (export "fc_alloc") (param i32) (result i32) (i32.const 0))
        (func (export "fc_call") (param i32 i32 i32) (result i64) (i64.const 0)))"#;
    let wasm = wat::parse_str(src).unwrap();
    assert!(matches!(
        instance(&wasm, manifest("", "")),
        Err(Fault::Abi(_))
    ));
}
