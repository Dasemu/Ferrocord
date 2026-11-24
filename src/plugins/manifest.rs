//! `plugin.toml`: identity, permissions, subscriptions, settings schema,
//! commands and context-menu actions. Everything a plugin can do is
//! declared here, so the host can show it before running any plugin code.

use std::collections::BTreeMap;

use ferrocord_plugin_sdk::abi::{ABI_VERSION, EVENT_NAMES, SettingValue};
use serde::{Deserialize, Serialize};

/// Largest `plugin.toml` we parse.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_SETTINGS: usize = 32;
const MAX_COMMANDS: usize = 16;
const MAX_ACTIONS: usize = 4;
const MAX_HOSTS: usize = 16;
const MAX_CHOICES: usize = 16;
/// Upper bound of a string setting.
const MAX_STRING_SETTING: usize = 4000;

/// A capability a plugin asks for. Calls that need one fail with
/// `err::DENIED` unless the user granted it when enabling the plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    ReadMessages,
    ReadPresence,
    TransformOutgoing,
    SendMessages,
    Reactions,
    Clipboard,
    Composer,
    Storage,
    Network,
    Decorations,
}

impl Permission {
    /// What the permission prompt says.
    pub fn describe(self) -> &'static str {
        match self {
            Self::ReadMessages => "Read messages in the channels you open",
            Self::ReadPresence => "See when people come online or go idle",
            Self::TransformOutgoing => "Change the text of messages before you send them",
            Self::SendMessages => {
                "Send messages as you, only when you run its command or menu item"
            }
            Self::Reactions => "Add reactions as you, only when you pick its menu item",
            Self::Clipboard => "Copy text to the clipboard when you pick its menu item",
            Self::Composer => "Insert text into your message box when you ask it to",
            Self::Storage => "Keep a small private store on this computer",
            Self::Network => "Connect to the internet hosts listed below",
            Self::Decorations => "Add badges and notes under messages",
        }
    }

    /// Permissions that act on the account or leave the machine.
    pub fn sensitive(self) -> bool {
        matches!(
            self,
            Self::SendMessages | Self::Reactions | Self::Network | Self::ReadMessages
        )
    }
}

/// Type and constraints of one setting.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingKind {
    Bool,
    Int { min: i64, max: i64 },
    Str { max_len: usize },
    Choice { choices: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct SettingSpec {
    pub key: String,
    pub label: String,
    pub description: String,
    pub kind: SettingKind,
    pub default: SettingValue,
}

impl SettingSpec {
    /// `v` if it fits this setting, else why not.
    pub fn check(&self, v: &SettingValue) -> Result<(), String> {
        match (&self.kind, v) {
            (SettingKind::Bool, SettingValue::Bool(_)) => Ok(()),
            (SettingKind::Int { min, max }, SettingValue::Int(i)) if (*min..=*max).contains(i) => {
                Ok(())
            }
            (SettingKind::Int { min, max }, SettingValue::Int(_)) => {
                Err(format!("must be between {min} and {max}"))
            }
            (SettingKind::Str { max_len }, SettingValue::Str(s))
                if s.chars().count() <= *max_len =>
            {
                Ok(())
            }
            (SettingKind::Str { max_len }, SettingValue::Str(_)) => {
                Err(format!("at most {max_len} characters"))
            }
            (SettingKind::Choice { choices }, SettingValue::Str(s)) if choices.contains(s) => {
                Ok(())
            }
            _ => Err("wrong type".into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandSpec {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionSpec {
    pub id: String,
    pub label: String,
}

/// A validated manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub api_version: u32,
    /// Sorted, without duplicates.
    pub permissions: Vec<Permission>,
    /// Exact lowercase host names `http_fetch` may reach.
    pub network_hosts: Vec<String>,
    pub events: Vec<String>,
    pub settings: Vec<SettingSpec>,
    pub commands: Vec<CommandSpec>,
    pub actions: Vec<ActionSpec>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    description: String,
    api_version: u32,
    #[serde(default)]
    permissions: Vec<Permission>,
    #[serde(default)]
    network_hosts: Vec<String>,
    #[serde(default)]
    events: Vec<String>,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    commands: Vec<RawCommand>,
    #[serde(default)]
    actions: Vec<RawAction>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    label: String,
    #[serde(default)]
    description: String,
    default: toml::Value,
    min: Option<i64>,
    max: Option<i64>,
    max_len: Option<usize>,
    #[serde(default)]
    choices: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    name: String,
    #[serde(default)]
    description: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAction {
    id: String,
    label: String,
}

/// `[a-z0-9]` then `[a-z0-9._-]`, 1..=max bytes.
fn valid_ident(s: &str, max: usize, extra: &[char]) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || extra.contains(&c))
}

fn short(field: &str, s: &str, max: usize) -> Result<String, String> {
    let s = s.trim();
    if s.chars().count() > max {
        return Err(format!("`{field}` is longer than {max} characters"));
    }
    if s.chars().any(char::is_control) {
        return Err(format!("`{field}` contains control characters"));
    }
    Ok(s.to_owned())
}

/// A bare DNS name: no scheme, port, path, wildcard or IP literal.
pub fn valid_host(h: &str) -> bool {
    h.len() <= 253
        && h.contains('.')
        && !h.starts_with('.')
        && !h.ends_with('.')
        && h.split('.').all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && !l.starts_with('-')
                && !l.ends_with('-')
                && l.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        })
        && !h
            .rsplit('.')
            .next()
            .is_some_and(|tld| tld.chars().all(|c| c.is_ascii_digit()))
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_MANIFEST_BYTES {
            return Err("plugin.toml is too large".into());
        }
        let raw: Raw = toml::from_str(text).map_err(|e| format!("plugin.toml: {}", e.message()))?;
        if !valid_ident(&raw.id, 64, &['.', '_', '-']) {
            return Err("`id` must be 1-64 characters of a-z, 0-9, '.', '_' or '-'".into());
        }
        if raw.api_version != ABI_VERSION {
            return Err(format!(
                "built for plugin API {}, this Ferrocord supports {ABI_VERSION}",
                raw.api_version
            ));
        }
        let name = short("name", &raw.name, 64)?;
        if name.is_empty() {
            return Err("`name` is empty".into());
        }
        let mut permissions = raw.permissions;
        permissions.sort();
        permissions.dedup();
        let has = |p| permissions.contains(&p);

        if raw.network_hosts.len() > MAX_HOSTS {
            return Err(format!("at most {MAX_HOSTS} `network_hosts`"));
        }
        let mut hosts = Vec::new();
        for h in &raw.network_hosts {
            if !valid_host(h) {
                return Err(format!(
                    "network host {h:?} must be a lowercase domain name (no scheme, port, IP or wildcard)"
                ));
            }
            if !hosts.contains(h) {
                hosts.push(h.clone());
            }
        }
        if has(Permission::Network) != !hosts.is_empty() {
            return Err("the `network` permission and `network_hosts` go together".into());
        }

        let mut events = Vec::new();
        for e in &raw.events {
            if !EVENT_NAMES.contains(&e.as_str()) {
                return Err(format!("unknown event {e:?}"));
            }
            let needs = match e.as_str() {
                "presence" => Some(Permission::ReadPresence),
                "channel_switch" => None,
                _ => Some(Permission::ReadMessages),
            };
            if let Some(p) = needs
                && !has(p)
            {
                return Err(format!("event {e:?} needs the {p:?} permission"));
            }
            if !events.contains(e) {
                events.push(e.clone());
            }
        }

        if raw.settings.len() > MAX_SETTINGS {
            return Err(format!("at most {MAX_SETTINGS} settings"));
        }
        let mut settings: Vec<SettingSpec> = Vec::new();
        for s in raw.settings {
            if !valid_ident(&s.key, 32, &['_']) || settings.iter().any(|o| o.key == s.key) {
                return Err(format!("bad or duplicate setting key {:?}", s.key));
            }
            let kind = match s.kind.as_str() {
                "bool" => SettingKind::Bool,
                "int" => {
                    let (min, max) = (s.min.unwrap_or(i64::MIN), s.max.unwrap_or(i64::MAX));
                    if min > max {
                        return Err(format!("setting {:?}: min > max", s.key));
                    }
                    SettingKind::Int { min, max }
                }
                "string" => SettingKind::Str {
                    max_len: s.max_len.unwrap_or(200).min(MAX_STRING_SETTING),
                },
                "choice" => {
                    if s.choices.is_empty() || s.choices.len() > MAX_CHOICES {
                        return Err(format!("setting {:?}: 1-{MAX_CHOICES} choices", s.key));
                    }
                    for c in &s.choices {
                        short("choice", c, 40)?;
                    }
                    SettingKind::Choice { choices: s.choices }
                }
                other => return Err(format!("setting {:?}: unknown type {other:?}", s.key)),
            };
            let default = match s.default {
                toml::Value::Boolean(b) => SettingValue::Bool(b),
                toml::Value::Integer(i) => SettingValue::Int(i),
                toml::Value::String(st) => SettingValue::Str(st),
                _ => return Err(format!("setting {:?}: unsupported default", s.key)),
            };
            let spec = SettingSpec {
                label: short("label", &s.label, 64)?,
                description: short("description", &s.description, 200)?,
                key: s.key,
                kind,
                default,
            };
            spec.check(&spec.default)
                .map_err(|e| format!("setting {:?}: default {e}", spec.key))?;
            settings.push(spec);
        }

        if raw.commands.len() > MAX_COMMANDS {
            return Err(format!("at most {MAX_COMMANDS} commands"));
        }
        let mut commands: Vec<CommandSpec> = Vec::new();
        for c in raw.commands {
            if !valid_ident(&c.name, 32, &['_', '-']) || commands.iter().any(|o| o.name == c.name) {
                return Err(format!("bad or duplicate command {:?}", c.name));
            }
            commands.push(CommandSpec {
                description: short("description", &c.description, 100)?,
                name: c.name,
            });
        }
        if raw.actions.len() > MAX_ACTIONS {
            return Err(format!("at most {MAX_ACTIONS} actions"));
        }
        let mut actions: Vec<ActionSpec> = Vec::new();
        for a in raw.actions {
            if !valid_ident(&a.id, 32, &['_', '-']) || actions.iter().any(|o| o.id == a.id) {
                return Err(format!("bad or duplicate action {:?}", a.id));
            }
            let label = short("label", &a.label, 40)?;
            if label.is_empty() {
                return Err("an action has an empty label".into());
            }
            actions.push(ActionSpec { id: a.id, label });
        }

        Ok(Self {
            id: raw.id,
            name,
            version: short("version", &raw.version, 32)?,
            author: short("author", &raw.author, 64)?,
            description: short("description", &raw.description, 500)?,
            api_version: raw.api_version,
            permissions,
            network_hosts: hosts,
            events,
            settings,
            commands,
            actions,
        })
    }

    pub fn has(&self, p: Permission) -> bool {
        self.permissions.contains(&p)
    }

    pub fn setting(&self, key: &str) -> Option<&SettingSpec> {
        self.settings.iter().find(|s| s.key == key)
    }

    /// Stored values, with defaults for missing or no-longer-valid ones.
    pub fn effective_settings(
        &self,
        stored: &BTreeMap<String, SettingValue>,
    ) -> BTreeMap<String, SettingValue> {
        self.settings
            .iter()
            .map(|s| {
                let v = stored
                    .get(&s.key)
                    .filter(|v| s.check(v).is_ok())
                    .cloned()
                    .unwrap_or_else(|| s.default.clone());
                (s.key.clone(), v)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
id = "demo"
name = "Demo"
version = "1.0.0"
api_version = 1
permissions = ["network", "read_messages", "network"]
network_hosts = ["api.example.com"]
events = ["message_create", "channel_switch"]

[[settings]]
key = "n"
type = "int"
label = "N"
default = 3
min = 1
max = 5

[[settings]]
key = "mode"
type = "choice"
label = "Mode"
choices = ["a", "b"]
default = "b"

[[commands]]
name = "hello"

[[actions]]
id = "x"
label = "Do X"
"#;

    #[test]
    fn parses_a_full_manifest() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!(m.id, "demo");
        assert_eq!(
            m.permissions,
            [Permission::ReadMessages, Permission::Network]
        );
        assert_eq!(m.network_hosts, ["api.example.com"]);
        assert_eq!(m.settings.len(), 2);
        assert!(
            m.setting("n")
                .unwrap()
                .check(&SettingValue::Int(6))
                .is_err()
        );
        let mut stored = BTreeMap::new();
        stored.insert("n".into(), SettingValue::Int(99));
        let eff = m.effective_settings(&stored);
        assert_eq!(eff["n"], SettingValue::Int(3));
        assert_eq!(eff["mode"], SettingValue::Str("b".into()));
    }

    #[test]
    fn rejects_bad_manifests() {
        let bad = [
            GOOD.replace("id = \"demo\"", "id = \"Demo!\""),
            GOOD.replace("api_version = 1", "api_version = 2"),
            GOOD.replace("\"api.example.com\"", "\"*.example.com\""),
            GOOD.replace("\"api.example.com\"", "\"https://api.example.com\""),
            GOOD.replace("\"api.example.com\"", "\"127.0.0.1\""),
            GOOD.replace("network_hosts = [\"api.example.com\"]", ""),
            GOOD.replace("\"read_messages\", ", ""),
            GOOD.replace("\"message_create\"", "\"gateway_raw\""),
            GOOD.replace("default = 3", "default = 9"),
            GOOD.replace("permissions = [", "token = true\npermissions = ["),
            GOOD.replace("\"network\", \"read", "\"read_token\", \"read"),
        ];
        for b in bad {
            assert!(Manifest::parse(&b).is_err(), "accepted:\n{b}");
        }
    }

    #[test]
    fn hosts() {
        assert!(valid_host("api.example.com"));
        assert!(!valid_host("localhost"));
        assert!(!valid_host("API.example.com"));
        assert!(!valid_host("example.com:443"));
        assert!(!valid_host("10.0.0.1"));
        assert!(!valid_host("-a.com"));
    }

    #[test]
    fn example_manifests_are_valid() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/plugins");
        for p in ["text-replacer", "word-highlighter", "quote-message"] {
            let t = std::fs::read_to_string(root.join(p).join("plugin.toml")).unwrap();
            let m = Manifest::parse(&t).unwrap();
            assert_eq!(m.id, p);
        }
    }
}
