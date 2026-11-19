//! Server folders: the grouping of servers in the rail.
//!
//! Pure reducers over the list the user settings carry in `guild_folders`
//! (`id`, `name`, `color`, `guild_ids`). A folder entry with `id == None` is
//! a single, ungrouped server (that is how the official client stores the
//! plain rail order). Persisting goes through
//! [`Folders::to_wire`] and `RestClient::set_guild_folders`.

use std::collections::HashSet;

use serde_json::{Value, json};

use crate::net::model::{GuildFolder, GuildId};

/// Folder colours offered in the folder settings (RGB).
pub const FOLDER_COLORS: [u32; 8] = [
    0x5865F2, 0x3BA55D, 0xFAA81A, 0xED4245, 0xEB459E, 0x9B59B6, 0x1ABC9C, 0x95A5A6,
];

/// Default folder colour (blurple) when none was picked.
pub const DEFAULT_COLOR: u32 = FOLDER_COLORS[0];

/// Folder ids the official client generates are positive 32-bit-ish ints.
pub fn new_folder_id() -> u64 {
    u64::from(fastrand::u32(1..))
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Folders {
    pub items: Vec<GuildFolder>,
}

impl Folders {
    /// Builds the list from user settings; `order` (legacy `guild_positions`)
    /// is used when there are no folders.
    pub fn from_settings(folders: &[GuildFolder], order: &[GuildId]) -> Self {
        let mut items: Vec<GuildFolder> = folders
            .iter()
            .filter(|f| !f.guild_ids.is_empty())
            .cloned()
            .collect();
        if items.is_empty() {
            items = order.iter().map(|g| GuildFolder::single(*g)).collect();
        }
        Self { items }
    }

    /// Guilds in display order (folders flattened).
    pub fn flat_order(&self) -> Vec<GuildId> {
        self.items
            .iter()
            .flat_map(|f| f.guild_ids.iter().copied())
            .collect()
    }

    /// Makes the list match the guilds we know: servers that are in no
    /// folder (just joined) are appended as singles, like the official
    /// client. Unknown ids stay in the list so saving never drops a server
    /// we have not loaded yet.
    pub fn sync(&mut self, known: &[GuildId]) {
        let present: HashSet<GuildId> = self.flat_order().into_iter().collect();
        let fresh: Vec<GuildFolder> = known
            .iter()
            .filter(|g| !present.contains(g))
            .map(|g| GuildFolder::single(*g))
            .collect();
        self.items.extend(fresh);
    }

    /// Drops a server we left (an emptied folder disappears too).
    pub fn forget(&mut self, g: GuildId) {
        self.take(g);
    }

    pub fn folder(&self, id: u64) -> Option<&GuildFolder> {
        self.items.iter().find(|f| f.id == Some(id))
    }

    /// The real folder containing `g`, if any.
    pub fn folder_of(&self, g: GuildId) -> Option<&GuildFolder> {
        self.items
            .iter()
            .find(|f| f.id.is_some() && f.guild_ids.contains(&g))
    }

    fn position_of(&self, g: GuildId) -> Option<usize> {
        self.items.iter().position(|f| f.guild_ids.contains(&g))
    }

    /// Removes `g` from wherever it is; empty entries disappear. Returns
    /// whether it was found.
    fn take(&mut self, g: GuildId) -> bool {
        let Some(i) = self.position_of(g) else {
            return false;
        };
        self.items[i].guild_ids.retain(|x| *x != g);
        if self.items[i].guild_ids.is_empty() {
            self.items.remove(i);
        }
        true
    }

    /// Drag `dragged` onto `target`. Onto a single server: a new folder with
    /// both; onto a folder member: joins that folder. Returns the id of the
    /// folder that now holds `dragged`.
    pub fn drop_on(&mut self, dragged: GuildId, target: GuildId, new_id: u64) -> Option<u64> {
        if dragged == target || self.position_of(dragged).is_none() {
            return None;
        }
        let target_folder = self.folder_of(target).and_then(|f| f.id);
        self.take(dragged);
        match target_folder {
            Some(id) => {
                let f = self.items.iter_mut().find(|f| f.id == Some(id))?;
                f.guild_ids.push(dragged);
                Some(id)
            }
            None => {
                let i = self.position_of(target)?;
                self.items[i] = GuildFolder {
                    id: Some(new_id),
                    name: None,
                    color: None,
                    guild_ids: vec![target, dragged],
                };
                Some(new_id)
            }
        }
    }

    /// Drag `dragged` onto a folder's header: appends it.
    pub fn drop_on_folder(&mut self, dragged: GuildId, folder: u64) -> bool {
        if self.folder(folder).is_none() || self.position_of(dragged).is_none() {
            return false;
        }
        if self
            .folder(folder)
            .is_some_and(|f| f.guild_ids.contains(&dragged))
        {
            return false;
        }
        self.take(dragged);
        match self.items.iter_mut().find(|f| f.id == Some(folder)) {
            Some(f) => {
                f.guild_ids.push(dragged);
                true
            }
            None => false,
        }
    }

    /// "Create Folder" in a server's menu: a folder holding just `g`.
    pub fn create_with(&mut self, g: GuildId, new_id: u64) -> Option<u64> {
        let i = self.position_of(g)?;
        if self.items[i].id.is_some() {
            return None;
        }
        self.items[i] = GuildFolder {
            id: Some(new_id),
            name: None,
            color: None,
            guild_ids: vec![g],
        };
        Some(new_id)
    }

    /// "Remove from folder": the server becomes a single right after the
    /// folder (an emptied folder is deleted).
    pub fn remove_from_folder(&mut self, g: GuildId) -> bool {
        let Some(i) = self
            .items
            .iter()
            .position(|f| f.id.is_some() && f.guild_ids.contains(&g))
        else {
            return false;
        };
        self.items[i].guild_ids.retain(|x| *x != g);
        let at = if self.items[i].guild_ids.is_empty() {
            self.items.remove(i);
            i
        } else {
            i + 1
        };
        self.items.insert(at, GuildFolder::single(g));
        true
    }

    /// Dissolves a folder into singles, keeping the order.
    pub fn ungroup(&mut self, folder: u64) -> bool {
        let Some(i) = self.items.iter().position(|f| f.id == Some(folder)) else {
            return false;
        };
        let f = self.items.remove(i);
        for (n, g) in f.guild_ids.into_iter().enumerate() {
            self.items.insert(i + n, GuildFolder::single(g));
        }
        true
    }

    /// Empty or blank names clear the name.
    pub fn rename(&mut self, folder: u64, name: &str) -> bool {
        let name = name.trim();
        match self.items.iter_mut().find(|f| f.id == Some(folder)) {
            Some(f) => {
                f.name = (!name.is_empty()).then(|| name.chars().take(32).collect());
                true
            }
            None => false,
        }
    }

    pub fn set_color(&mut self, folder: u64, color: Option<u32>) -> bool {
        match self.items.iter_mut().find(|f| f.id == Some(folder)) {
            Some(f) => {
                f.color = color;
                true
            }
            None => false,
        }
    }

    /// Body of `PATCH /users/@me/settings`.
    pub fn to_wire(&self) -> Value {
        let folders: Vec<Value> = self
            .items
            .iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "name": f.name,
                    "color": f.color,
                    "guild_ids": f.guild_ids.iter().map(|g| g.to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "guild_folders": folders })
    }
}

/// Display name of a folder: its name, or the member names joined like the
/// official client does for unnamed folders.
pub fn folder_label<'a>(f: &GuildFolder, name_of: impl Fn(GuildId) -> Option<&'a str>) -> String {
    if let Some(n) = f.name.as_deref().filter(|n| !n.is_empty()) {
        return n.to_owned();
    }
    let names: Vec<&str> = f.guild_ids.iter().filter_map(|g| name_of(*g)).collect();
    if names.is_empty() {
        "Unnamed folder".into()
    } else {
        names.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::model::Snowflake;

    fn g(n: u64) -> GuildId {
        Snowflake(n)
    }

    fn rail(ids: &[u64]) -> Folders {
        Folders {
            items: ids.iter().map(|n| GuildFolder::single(g(*n))).collect(),
        }
    }

    fn order(f: &Folders) -> Vec<u64> {
        f.flat_order().into_iter().map(|g| g.0).collect()
    }

    #[test]
    fn from_settings_falls_back_to_the_legacy_order() {
        let f = Folders::from_settings(&[], &[g(3), g(1)]);
        assert_eq!(order(&f), vec![3, 1]);
        assert!(f.items.iter().all(|f| f.id.is_none()));
        let folders = vec![
            GuildFolder::single(g(9)),
            GuildFolder {
                id: Some(7),
                name: Some("Dev".into()),
                color: Some(0xff0000),
                guild_ids: vec![g(1), g(2)],
            },
        ];
        let f = Folders::from_settings(&folders, &[g(5)]);
        assert_eq!(order(&f), vec![9, 1, 2]);
    }

    #[test]
    fn dropping_a_server_on_another_creates_a_folder_in_place() {
        let mut f = rail(&[1, 2, 3, 4]);
        assert_eq!(f.drop_on(g(4), g(2), 77), Some(77));
        assert_eq!(order(&f), vec![1, 2, 4, 3]);
        let folder = f.folder(77).unwrap();
        assert_eq!(folder.guild_ids, vec![g(2), g(4)]);
        assert_eq!(f.items.len(), 3);
        // Dropping on itself does nothing.
        assert_eq!(f.drop_on(g(1), g(1), 78), None);
    }

    #[test]
    fn dropping_on_a_folder_member_joins_that_folder() {
        let mut f = rail(&[1, 2, 3]);
        f.drop_on(g(2), g(1), 10);
        assert_eq!(f.drop_on(g(3), g(2), 11), Some(10));
        assert_eq!(f.folder(10).unwrap().guild_ids, vec![g(1), g(2), g(3)]);
        assert_eq!(f.items.len(), 1);
    }

    #[test]
    fn moving_the_last_member_out_deletes_the_old_folder() {
        let mut f = rail(&[1, 2, 3]);
        f.create_with(g(1), 10);
        // 1 is alone in folder 10; dragging it onto 2 empties the folder.
        assert_eq!(f.drop_on(g(1), g(2), 11), Some(11));
        assert!(f.folder(10).is_none());
        assert_eq!(order(&f), vec![2, 1, 3]);
    }

    #[test]
    fn drop_on_folder_header_appends() {
        let mut f = rail(&[1, 2, 3]);
        f.drop_on(g(2), g(1), 10);
        assert!(f.drop_on_folder(g(3), 10));
        assert_eq!(f.folder(10).unwrap().guild_ids, vec![g(1), g(2), g(3)]);
        // Already inside: no change.
        assert!(!f.drop_on_folder(g(3), 10));
        assert!(!f.drop_on_folder(g(3), 99));
    }

    #[test]
    fn create_with_only_wraps_single_servers() {
        let mut f = rail(&[1, 2]);
        assert_eq!(f.create_with(g(2), 5), Some(5));
        assert_eq!(f.create_with(g(2), 6), None);
        assert_eq!(f.create_with(g(9), 7), None);
        assert_eq!(f.folder_of(g(2)).unwrap().id, Some(5));
        assert!(f.folder_of(g(1)).is_none());
    }

    #[test]
    fn removing_from_a_folder_places_the_server_after_it() {
        let mut f = rail(&[1, 2, 3]);
        f.drop_on(g(2), g(1), 10);
        assert!(f.remove_from_folder(g(1)));
        assert_eq!(order(&f), vec![2, 1, 3]);
        // 2 stays in a one-member folder; removing it deletes the folder.
        assert!(f.remove_from_folder(g(2)));
        assert!(f.folder(10).is_none());
        assert_eq!(order(&f), vec![2, 1, 3]);
        assert!(!f.remove_from_folder(g(3)));
    }

    #[test]
    fn ungroup_keeps_order() {
        let mut f = rail(&[1, 2, 3]);
        f.drop_on(g(3), g(1), 10);
        assert!(f.ungroup(10));
        assert_eq!(order(&f), vec![1, 3, 2]);
        assert!(f.items.iter().all(|f| f.id.is_none()));
        assert!(!f.ungroup(10));
    }

    #[test]
    fn rename_and_colour() {
        let mut f = rail(&[1, 2]);
        f.drop_on(g(2), g(1), 10);
        assert!(f.rename(10, "  Work  "));
        assert_eq!(f.folder(10).unwrap().name.as_deref(), Some("Work"));
        assert!(f.rename(10, "   "));
        assert_eq!(f.folder(10).unwrap().name, None);
        assert!(!f.rename(11, "x"));
        assert!(f.rename(10, &"x".repeat(100)));
        assert_eq!(f.folder(10).unwrap().name.as_ref().unwrap().len(), 32);
        assert!(f.set_color(10, Some(0x3BA55D)));
        assert_eq!(f.folder(10).unwrap().color, Some(0x3BA55D));
        assert!(f.set_color(10, None));
        assert!(!f.set_color(99, None));
    }

    #[test]
    fn sync_appends_new_servers_and_keeps_unknown_ones() {
        let mut f = rail(&[1, 2, 99]);
        f.sync(&[g(1), g(2), g(5)]);
        assert_eq!(order(&f), vec![1, 2, 99, 5]);
        f.sync(&[g(1), g(2), g(5)]);
        assert_eq!(order(&f), vec![1, 2, 99, 5]);
        f.forget(g(2));
        f.forget(g(77));
        assert_eq!(order(&f), vec![1, 99, 5]);
    }

    #[test]
    fn wire_format_matches_the_settings_payload() {
        let mut f = rail(&[1, 2]);
        f.drop_on(g(2), g(1), 10);
        f.rename(10, "Dev");
        f.set_color(10, Some(0x5865F2));
        let v = f.to_wire();
        assert_eq!(
            v,
            json!({"guild_folders": [
                {"id": 10, "name": "Dev", "color": 0x5865F2, "guild_ids": ["1", "2"]}
            ]})
        );
        let single = rail(&[4]).to_wire();
        assert_eq!(
            single["guild_folders"][0],
            json!({"id": null, "name": null, "color": null, "guild_ids": ["4"]})
        );
    }

    #[test]
    fn label_uses_member_names_for_unnamed_folders() {
        let f = GuildFolder {
            id: Some(1),
            name: None,
            color: None,
            guild_ids: vec![g(1), g(2), g(3)],
        };
        let names = |id: GuildId| match id.0 {
            1 => Some("Rust"),
            2 => Some("Iced"),
            _ => None,
        };
        assert_eq!(folder_label(&f, names), "Rust, Iced");
        let named = GuildFolder {
            name: Some("Dev".into()),
            ..f.clone()
        };
        assert_eq!(folder_label(&named, names), "Dev");
        assert_eq!(folder_label(&f, |_| None), "Unnamed folder");
    }
}
