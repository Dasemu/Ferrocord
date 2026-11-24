//! The host side of `ferrocord.host_call`: permission and user-action
//! checks, limits, and the requests handed back to the app.
//!
//! Nothing here touches the network, the token, the keyring or arbitrary
//! files. Calls that act on the account or the outside world only produce
//! an [`Out`] request; the app performs it through its own typed code paths
//! (the same REST calls, rate limits and allowlists as the rest of the app).

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use ferrocord_plugin_sdk::abi::{
    self, DecorateReq, Decoration, HttpRequest, KeyReq, KvSetReq, LogReq, SetSettingReq,
    SettingValue, TextReq, err, op,
};
use wasmi::StoreLimits;

use super::kv::Kv;
use super::manifest::{Manifest, Permission};
use crate::error::Redacted;

/// Host calls one hook invocation may make.
pub const MAX_CALLS_PER_HOOK: u32 = 256;
/// Largest host-call input.
pub const MAX_CALL_INPUT: usize = 256 * 1024;
/// A user action's grant (send, react, ...) lasts this long, enough for
/// one HTTP round trip started by the action.
pub const GRANT_TTL_MS: u64 = 30_000;
pub const MAX_DECORATIONS: usize = 4;
pub const MAX_DECORATION_TEXT: usize = 100;
pub const MAX_TOOLTIP: usize = 200;
pub const MAX_TOAST: usize = 200;
const MAX_LOG: usize = 1000;
pub const MAX_HTTP_BODY: usize = 64 * 1024;
const MAX_HTTP_HEADERS: usize = 16;
/// Header names a plugin may not set (the client owns these).
const FORBIDDEN_HEADERS: [&str; 9] = [
    "host",
    "cookie",
    "connection",
    "content-length",
    "transfer-encoding",
    "upgrade",
    "proxy-authorization",
    "te",
    "trailer",
];

/// Permission to act for the user, created only by an explicit user
/// action (a `/command` or a context-menu item) and scoped to its channel
/// (and message). One message may be sent per grant.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Grant {
    pub id: u64,
    pub channel: u64,
    pub guild: Option<u64>,
    pub message: Option<u64>,
    pub expires_ms: u64,
}

impl std::fmt::Debug for Grant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Grant({})", self.id)
    }
}

/// A request from a plugin to the app. `Debug` never prints content.
#[derive(Clone, PartialEq)]
pub enum Out {
    /// A plugin finished loading (or failed to).
    Loaded {
        id: String,
        error: Option<String>,
    },
    /// The plugin trapped, ran out of fuel or panicked.
    Faulted {
        id: String,
        reason: String,
        disabled: bool,
    },
    Toast {
        id: String,
        text: String,
    },
    /// Result of a transform ticket (`None`: send the text unchanged).
    Transformed {
        ticket: u64,
        text: Option<String>,
    },
    Send {
        id: String,
        channel: u64,
        content: String,
    },
    React {
        id: String,
        channel: u64,
        message: u64,
        emoji: String,
    },
    Clipboard {
        id: String,
        text: String,
    },
    Composer {
        id: String,
        text: String,
    },
    Decorate {
        id: String,
        message: u64,
        decorations: Vec<Decoration>,
    },
    Http {
        id: String,
        request: u64,
        req: HttpRequest,
        grant: Option<Grant>,
    },
    SettingSaved {
        id: String,
        key: String,
        value: SettingValue,
    },
}

impl std::fmt::Debug for Out {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Loaded { .. } => "Loaded",
            Self::Faulted { .. } => "Faulted",
            Self::Toast { .. } => "Toast",
            Self::Transformed { .. } => "Transformed",
            Self::Send { .. } => "Send",
            Self::React { .. } => "React",
            Self::Clipboard { .. } => "Clipboard",
            Self::Composer { .. } => "Composer",
            Self::Decorate { .. } => "Decorate",
            Self::Http { .. } => "Http",
            Self::SettingSaved { .. } => "SettingSaved",
        };
        write!(f, "Out::{name}")
    }
}

/// What the hook being run may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// Load, unload, events, setting changes, HTTP answers without a grant.
    Passive,
    /// The outgoing transform: only settings, log and clock.
    Transform,
    /// A user action (or the HTTP answer of a request it started).
    User(Grant),
}

/// Per-instance host state (the wasmi store data).
pub struct Env {
    pub manifest: Arc<Manifest>,
    pub settings: BTreeMap<String, SettingValue>,
    pub kind: CallKind,
    /// Requests produced by the current call; forwarded when it returns
    /// cleanly, dropped if it faults.
    pub out: Vec<Out>,
    pub calls: u32,
    /// Result of the last `host_call`, read by `host_take`.
    pub pending: Vec<u8>,
    pub kv: Rc<RefCell<Kv>>,
    pub limits: StoreLimits,
    /// The instance's linear memory, set once it is instantiated (host
    /// calls look it up by name only before that, from a start function).
    pub memory: Option<wasmi::Memory>,
    pub now_ms: fn() -> u64,
    next_request: u64,
    /// Grants that already sent their message.
    spent: VecDeque<u64>,
    toasted: bool,
}

impl Env {
    pub fn new(
        manifest: Arc<Manifest>,
        settings: BTreeMap<String, SettingValue>,
        kv: Rc<RefCell<Kv>>,
        limits: StoreLimits,
        now_ms: fn() -> u64,
    ) -> Self {
        Self {
            manifest,
            settings,
            kind: CallKind::Passive,
            out: Vec::new(),
            calls: 0,
            pending: Vec::new(),
            kv,
            limits,
            memory: None,
            now_ms,
            next_request: 1,
            spent: VecDeque::new(),
            toasted: false,
        }
    }

    /// Resets the per-call budget before a hook runs.
    pub fn begin(&mut self, kind: CallKind) {
        self.kind = kind;
        self.calls = 0;
        self.out.clear();
        self.pending.clear();
        self.toasted = false;
    }

    /// Moves the persistent parts (settings, request ids, spent grants)
    /// into a fresh environment after a fault.
    pub fn carry_over(&mut self, old: Env) {
        self.settings = old.settings;
        self.next_request = old.next_request;
        self.spent = old.spent;
    }

    fn id(&self) -> String {
        self.manifest.id.clone()
    }

    fn need(&self, p: Permission) -> Result<(), i32> {
        if self.manifest.has(p) {
            Ok(())
        } else {
            Err(err::DENIED)
        }
    }

    /// The live grant of the current user action.
    fn grant(&self) -> Result<Grant, i32> {
        match self.kind {
            CallKind::User(g) if (self.now_ms)() <= g.expires_ms => Ok(g),
            _ => Err(err::NOT_USER_ACTION),
        }
    }
}

fn arg<'a, T: serde::Deserialize<'a>>(input: &'a [u8]) -> Result<T, i32> {
    abi::decode(input).map_err(|_| err::BAD_INPUT)
}

fn ok<T: serde::Serialize + ?Sized>(v: &T) -> Result<Vec<u8>, i32> {
    abi::encode(v).map_err(|_| err::BAD_INPUT)
}

const NONE: Result<Vec<u8>, i32> = Ok(Vec::new());

/// Trims and checks a text: non-empty, at most `max` chars, no control
/// characters other than newlines and tabs.
fn text_in(s: &str, max: usize) -> Result<String, i32> {
    let s = s.trim();
    if s.is_empty() || s.chars().any(|c| c.is_control() && c != '\n' && c != '\t') {
        return Err(err::BAD_INPUT);
    }
    if s.chars().count() > max {
        return Err(err::LIMIT);
    }
    Ok(s.to_owned())
}

/// Runs one host call.
pub fn handle(env: &mut Env, op_id: i32, input: &[u8]) -> Result<Vec<u8>, i32> {
    env.calls += 1;
    if env.calls > MAX_CALLS_PER_HOOK || input.len() > MAX_CALL_INPUT {
        return Err(err::LIMIT);
    }
    // The transform runs while the user waits: pure calls only.
    if env.kind == CallKind::Transform && !matches!(op_id, op::LOG | op::GET_SETTING | op::NOW_MS) {
        return Err(err::DENIED);
    }
    match op_id {
        op::LOG => {
            let r: LogReq = arg(input)?;
            let msg: String = r.message.chars().take(MAX_LOG).collect();
            let id = env.manifest.id.as_str();
            // Plugin output may hold message bodies: only shown on opt-in.
            if std::env::var_os("FERROCORD_PLUGIN_LOG").is_some() {
                match r.level {
                    0 => tracing::error!(plugin = id, "{msg}"),
                    1 => tracing::warn!(plugin = id, "{msg}"),
                    2 => tracing::info!(plugin = id, "{msg}"),
                    _ => tracing::debug!(plugin = id, "{msg}"),
                }
            } else if r.level <= 1 {
                tracing::warn!(plugin = id, message = %Redacted(&msg), "plugin log");
            }
            NONE
        }
        op::TOAST => {
            let r: TextReq = arg(input)?;
            if env.toasted {
                return Err(err::LIMIT);
            }
            let text = text_in(&r.text, MAX_TOAST)?;
            env.toasted = true;
            env.out.push(Out::Toast { id: env.id(), text });
            NONE
        }
        op::GET_SETTING => {
            let r: KeyReq = arg(input)?;
            ok(&env.settings.get(&r.key))
        }
        op::SET_SETTING => {
            let r: SetSettingReq = arg(input)?;
            let spec = env.manifest.setting(&r.key).ok_or(err::BAD_INPUT)?;
            spec.check(&r.value).map_err(|_| err::BAD_INPUT)?;
            env.settings.insert(r.key.clone(), r.value.clone());
            env.out.push(Out::SettingSaved {
                id: env.id(),
                key: r.key,
                value: r.value,
            });
            NONE
        }
        op::KV_GET => {
            env.need(Permission::Storage)?;
            let r: KeyReq = arg(input)?;
            let v = env.kv.borrow_mut().get(&env.manifest.id, &r.key)?;
            ok(&v.map(ferrocord_plugin_sdk::serde_bytes::ByteBuf::from))
        }
        op::KV_SET => {
            env.need(Permission::Storage)?;
            let r: KvSetReq = arg(input)?;
            env.kv
                .borrow_mut()
                .set(&env.manifest.id, &r.key, &r.value)?;
            NONE
        }
        op::KV_DELETE => {
            env.need(Permission::Storage)?;
            let r: KeyReq = arg(input)?;
            env.kv.borrow_mut().delete(&env.manifest.id, &r.key)?;
            NONE
        }
        op::HTTP_FETCH => {
            env.need(Permission::Network)?;
            let mut r: HttpRequest = arg(input)?;
            check_http(&env.manifest, &mut r)?;
            let request = env.next_request;
            env.next_request += 1;
            let grant = env.grant().ok();
            env.out.push(Out::Http {
                id: env.id(),
                request,
                req: r,
                grant,
            });
            ok(&request)
        }
        op::SEND_MESSAGE => {
            env.need(Permission::SendMessages)?;
            let g = env.grant()?;
            if env.spent.contains(&g.id) {
                return Err(err::LIMIT);
            }
            let r: TextReq = arg(input)?;
            let content = text_in(&r.text, 2000)?;
            env.spent.push_back(g.id);
            if env.spent.len() > 64 {
                env.spent.pop_front();
            }
            env.out.push(Out::Send {
                id: env.id(),
                channel: g.channel,
                content,
            });
            NONE
        }
        op::DECORATE => {
            env.need(Permission::Decorations)?;
            let r: DecorateReq = arg(input)?;
            if r.decorations.len() > MAX_DECORATIONS || r.message_id == 0 {
                return Err(err::LIMIT);
            }
            let mut decorations = Vec::with_capacity(r.decorations.len());
            for d in r.decorations {
                let text = d.text.trim();
                if text.chars().count() > MAX_DECORATION_TEXT || text.chars().any(char::is_control)
                {
                    return Err(err::LIMIT);
                }
                let tooltip = match d.tooltip.as_deref().map(str::trim) {
                    Some("") | None => None,
                    Some(t) => Some(text_in(t, MAX_TOOLTIP)?),
                };
                decorations.push(Decoration {
                    kind: d.kind,
                    text: text.to_owned(),
                    tooltip,
                });
            }
            env.out.push(Out::Decorate {
                id: env.id(),
                message: r.message_id,
                decorations,
            });
            NONE
        }
        op::CLIPBOARD_WRITE => {
            env.need(Permission::Clipboard)?;
            env.grant()?;
            let r: TextReq = arg(input)?;
            let text = text_in(&r.text, 8000)?;
            env.out.push(Out::Clipboard { id: env.id(), text });
            NONE
        }
        op::COMPOSER_INSERT => {
            env.need(Permission::Composer)?;
            env.grant()?;
            let r: TextReq = arg(input)?;
            let text = text_in(&r.text, 2000)?;
            env.out.push(Out::Composer { id: env.id(), text });
            NONE
        }
        op::ADD_REACTION => {
            env.need(Permission::Reactions)?;
            let g = env.grant()?;
            let message = g.message.ok_or(err::NOT_USER_ACTION)?;
            let r: TextReq = arg(input)?;
            if emojis::get(&r.text).is_none() {
                return Err(err::BAD_INPUT);
            }
            env.out.push(Out::React {
                id: env.id(),
                channel: g.channel,
                message,
                emoji: r.text,
            });
            NONE
        }
        op::NOW_MS => ok(&(env.now_ms)()),
        _ => Err(err::UNKNOWN_OP),
    }
}

/// HTTPS to an allowlisted host on the default port, small bodies, no
/// headers that belong to the client.
pub fn check_http(m: &Manifest, r: &mut HttpRequest) -> Result<(), i32> {
    r.method = r.method.to_ascii_uppercase();
    if !matches!(r.method.as_str(), "GET" | "POST") {
        return Err(err::BAD_INPUT);
    }
    let u = url::Url::parse(&r.url).map_err(|_| err::BAD_INPUT)?;
    let host = u.host_str().unwrap_or_default();
    if u.scheme() != "https"
        || !u.username().is_empty()
        || u.password().is_some()
        || u.port().is_some()
        || !m.network_hosts.iter().any(|h| h == host)
    {
        return Err(err::HOST_NOT_ALLOWED);
    }
    if r.body.len() > MAX_HTTP_BODY || r.headers.len() > MAX_HTTP_HEADERS {
        return Err(err::LIMIT);
    }
    for (k, v) in &r.headers {
        let k = k.to_ascii_lowercase();
        if k.is_empty()
            || FORBIDDEN_HEADERS.contains(&k.as_str())
            || k.starts_with("sec-")
            || k.starts_with("proxy-")
            || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || v.len() > 1024
            || v.chars().any(char::is_control)
        {
            return Err(err::BAD_INPUT);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(perms: &str, hosts: &str) -> Env {
        let m = Manifest::parse(&format!(
            "id = \"t\"\nname = \"T\"\nversion = \"1\"\napi_version = 1\npermissions = [{perms}]\nnetwork_hosts = [{hosts}]\n\
             [[settings]]\nkey = \"n\"\ntype = \"int\"\nlabel = \"N\"\ndefault = 1\nmin = 0\nmax = 9\n"
        ))
        .unwrap();
        let settings = m.effective_settings(&BTreeMap::new());
        Env::new(
            Arc::new(m),
            settings,
            Rc::new(RefCell::new(Kv::new(None))),
            wasmi::StoreLimitsBuilder::new().build(),
            || 1_000,
        )
    }

    fn grant() -> Grant {
        Grant {
            id: 1,
            channel: 5,
            guild: None,
            message: Some(9),
            expires_ms: 2_000,
        }
    }

    fn text(t: &str) -> Vec<u8> {
        abi::encode(&TextReq { text: t.into() }).unwrap()
    }

    #[test]
    fn calls_without_permission_are_denied() {
        let mut e = env("", "");
        let kv = abi::encode(&KeyReq { key: "k".into() }).unwrap();
        for (o, input) in [
            (op::KV_GET, kv.clone()),
            (op::KV_DELETE, kv),
            (op::SEND_MESSAGE, text("hi")),
            (op::CLIPBOARD_WRITE, text("hi")),
            (op::COMPOSER_INSERT, text("hi")),
            (op::ADD_REACTION, text("👍")),
            (
                op::DECORATE,
                abi::encode(&DecorateReq {
                    message_id: 1,
                    decorations: vec![],
                })
                .unwrap(),
            ),
            (
                op::HTTP_FETCH,
                abi::encode(&HttpRequest {
                    method: "GET".into(),
                    url: "https://api.example.com/".into(),
                    ..Default::default()
                })
                .unwrap(),
            ),
        ] {
            e.begin(CallKind::User(grant()));
            assert_eq!(handle(&mut e, o, &input), Err(err::DENIED), "op {o}");
        }
        assert!(e.out.is_empty());
    }

    #[test]
    fn send_message_needs_a_live_user_action_and_sends_once() {
        let mut e = env("\"send_messages\"", "");
        e.begin(CallKind::Passive);
        assert_eq!(
            handle(&mut e, op::SEND_MESSAGE, &text("hi")),
            Err(err::NOT_USER_ACTION)
        );
        e.begin(CallKind::Transform);
        assert_eq!(
            handle(&mut e, op::SEND_MESSAGE, &text("hi")),
            Err(err::DENIED)
        );
        // Expired grant.
        e.begin(CallKind::User(Grant {
            expires_ms: 999,
            ..grant()
        }));
        assert_eq!(
            handle(&mut e, op::SEND_MESSAGE, &text("hi")),
            Err(err::NOT_USER_ACTION)
        );
        e.begin(CallKind::User(grant()));
        assert_eq!(handle(&mut e, op::SEND_MESSAGE, &text(" hi ")), Ok(vec![]));
        assert_eq!(
            e.out,
            [Out::Send {
                id: "t".into(),
                channel: 5,
                content: "hi".into()
            }]
        );
        // Same grant again (also from a later HTTP answer): refused.
        e.begin(CallKind::User(grant()));
        assert_eq!(
            handle(&mut e, op::SEND_MESSAGE, &text("again")),
            Err(err::LIMIT)
        );
    }

    #[test]
    fn http_only_to_allowlisted_https_hosts() {
        let m = Manifest::parse(
            "id = \"t\"\nname = \"T\"\nversion = \"1\"\napi_version = 1\npermissions = [\"network\"]\nnetwork_hosts = [\"api.example.com\"]\n",
        )
        .unwrap();
        let req = |url: &str| HttpRequest {
            method: "get".into(),
            url: url.into(),
            ..Default::default()
        };
        assert_eq!(
            check_http(&m, &mut req("https://api.example.com/v1?q=1")),
            Ok(())
        );
        for bad in [
            "https://evil.example.com/",
            "https://api.example.com.evil.net/",
            "http://api.example.com/",
            "https://api.example.com:8443/",
            "https://user@api.example.com/",
            "https://discord.com/api/v9/users/@me",
            "file:///etc/passwd",
            "https://127.0.0.1/",
        ] {
            assert_eq!(
                check_http(&m, &mut req(bad)),
                Err(err::HOST_NOT_ALLOWED),
                "{bad}"
            );
        }
        let mut r = req("https://api.example.com/");
        r.headers.push(("Cookie".into(), "x".into()));
        assert_eq!(check_http(&m, &mut r), Err(err::BAD_INPUT));
        let mut r = req("https://api.example.com/");
        r.method = "DELETE".into();
        assert_eq!(check_http(&m, &mut r), Err(err::BAD_INPUT));
    }

    #[test]
    fn settings_are_schema_checked() {
        let mut e = env("", "");
        e.begin(CallKind::Passive);
        let set = |v| {
            abi::encode(&SetSettingReq {
                key: "n".into(),
                value: v,
            })
            .unwrap()
        };
        assert_eq!(
            handle(&mut e, op::SET_SETTING, &set(SettingValue::Int(10))),
            Err(err::BAD_INPUT)
        );
        assert_eq!(
            handle(&mut e, op::SET_SETTING, &set(SettingValue::Bool(true))),
            Err(err::BAD_INPUT)
        );
        assert!(handle(&mut e, op::SET_SETTING, &set(SettingValue::Int(4))).is_ok());
        let got = handle(
            &mut e,
            op::GET_SETTING,
            &abi::encode(&KeyReq { key: "n".into() }).unwrap(),
        )
        .unwrap();
        assert_eq!(
            abi::decode::<Option<SettingValue>>(&got).unwrap(),
            Some(SettingValue::Int(4))
        );
    }

    #[test]
    fn budgets_and_limits() {
        let mut e = env("\"decorations\"", "");
        e.begin(CallKind::Passive);
        let toast = text("hello");
        assert!(handle(&mut e, op::TOAST, &toast).is_ok());
        assert_eq!(handle(&mut e, op::TOAST, &toast), Err(err::LIMIT));
        let deco = |n: usize, t: &str| {
            abi::encode(&DecorateReq {
                message_id: 3,
                decorations: vec![
                    Decoration {
                        text: t.into(),
                        ..Default::default()
                    };
                    n
                ],
            })
            .unwrap()
        };
        assert_eq!(handle(&mut e, op::DECORATE, &deco(5, "x")), Err(err::LIMIT));
        assert_eq!(
            handle(&mut e, op::DECORATE, &deco(1, &"x".repeat(101))),
            Err(err::LIMIT)
        );
        assert!(handle(&mut e, op::DECORATE, &deco(1, "ok")).is_ok());
        assert_eq!(handle(&mut e, 999, &[]), Err(err::UNKNOWN_OP));
        for _ in 0..MAX_CALLS_PER_HOOK {
            let _ = handle(&mut e, op::NOW_MS, &[]);
        }
        assert_eq!(handle(&mut e, op::NOW_MS, &[]), Err(err::LIMIT));
    }
}
