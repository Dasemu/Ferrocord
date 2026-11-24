//! What the user decided about each plugin, kept in
//! `<plugins dir>/state.json`: enabled or not, the permissions and hosts
//! they granted, and setting values. Plugins are disabled by default.

use std::collections::BTreeMap;
use std::path::Path;

use ferrocord_plugin_sdk::abi::SettingValue;
use serde::{Deserialize, Serialize};

use super::manifest::{Manifest, Permission};

pub const STATE_FILE: &str = "state.json";
/// A state file larger than this is ignored (it is ours; it never gets big).
const MAX_STATE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Record {
    pub enabled: bool,
    /// Permissions granted when the user enabled the plugin.
    pub granted: Vec<Permission>,
    pub granted_hosts: Vec<String>,
    pub settings: BTreeMap<String, SettingValue>,
    /// Why the host turned it off (faults), shown on the Plugins page.
    pub disabled_reason: Option<String>,
}

impl Record {
    /// Whether the grant covers everything `m` asks for. A plugin update
    /// that asks for more must be approved again.
    pub fn covers(&self, m: &Manifest) -> bool {
        m.permissions.iter().all(|p| self.granted.contains(p))
            && m.network_hosts
                .iter()
                .all(|h| self.granted_hosts.contains(h))
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub plugins: BTreeMap<String, Record>,
}

impl State {
    /// Reads the state file; missing or unreadable means "nothing enabled".
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(STATE_FILE);
        let Ok(meta) = std::fs::metadata(&path) else {
            return Self::default();
        };
        if meta.len() > MAX_STATE_BYTES {
            tracing::warn!("plugins state file too large; ignoring it");
            return Self::default();
        }
        match std::fs::read(&path).map(|b| serde_json::from_slice(&b)) {
            Ok(Ok(s)) => s,
            _ => {
                tracing::warn!("plugins state file unreadable; all plugins stay disabled");
                Self::default()
            }
        }
    }

    /// Writes atomically (temp file + rename).
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("{STATE_FILE}.tmp"));
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(&tmp, json)?;
        std::fs::rename(tmp, dir.join(STATE_FILE))
    }

    pub fn any_enabled(&self) -> bool {
        self.plugins.values().any(|r| r.enabled)
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        self.plugins.get(id).is_some_and(|r| r.enabled)
    }

    /// Enables `m` with exactly the permissions it declares.
    pub fn enable(&mut self, m: &Manifest) {
        let r = self.plugins.entry(m.id.clone()).or_default();
        r.enabled = true;
        r.granted = m.permissions.clone();
        r.granted_hosts = m.network_hosts.clone();
        r.disabled_reason = None;
    }

    /// Disables `id`; the grant is dropped so enabling asks again.
    pub fn disable(&mut self, id: &str, reason: Option<String>) {
        if let Some(r) = self.plugins.get_mut(id) {
            r.enabled = false;
            r.granted.clear();
            r.granted_hosts.clear();
            r.disabled_reason = reason;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(perms: &str) -> Manifest {
        Manifest::parse(&format!(
            "id = \"p\"\nname = \"P\"\nversion = \"1\"\napi_version = 1\npermissions = [{perms}]\n"
        ))
        .unwrap()
    }

    #[test]
    fn roundtrip_and_grants() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(State::load(dir.path()), State::default());
        let mut s = State::default();
        let m = manifest("\"storage\"");
        s.enable(&m);
        s.plugins
            .get_mut("p")
            .unwrap()
            .settings
            .insert("k".into(), SettingValue::Int(2));
        s.save(dir.path()).unwrap();
        let back = State::load(dir.path());
        assert_eq!(back, s);
        assert!(back.any_enabled());
        assert!(back.plugins["p"].covers(&m));
        // An update asking for more is not covered.
        assert!(!back.plugins["p"].covers(&manifest("\"storage\", \"clipboard\"")));
        let mut s = back;
        s.disable("p", Some("faults".into()));
        assert!(!s.any_enabled());
        assert!(s.plugins["p"].granted.is_empty());
        // Settings survive disabling.
        assert_eq!(s.plugins["p"].settings["k"], SettingValue::Int(2));
    }

    #[test]
    fn garbage_means_nothing_enabled() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(STATE_FILE), "not json").unwrap();
        assert!(!State::load(dir.path()).any_enabled());
    }
}
