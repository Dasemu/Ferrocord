//! Local cache: SQLite via `rusqlite`.
//!
//! Rows store the domain types as JSON in a `data` column with the ids
//! broken out for indexing; this keeps migrations trivial while the domain
//! types evolve. The UI thread never waits on disk: everything except the
//! boot snapshot goes through [`Store::blocking`].

use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};

use rusqlite::{Connection, OptionalExtension, params};

use crate::error::{Error, Result};
use crate::net::model::*;

const SCHEMA_V1: &str = include_str!("schema.sql");
const SCHEMA_VERSION: i64 = 1;
/// Messages kept on disk per channel.
pub const MAX_MESSAGES_PER_CHANNEL: usize = 1000;

fn id(s: Snowflake) -> i64 {
    s.0 as i64
}

fn sf(v: i64) -> Snowflake {
    Snowflake(v as u64)
}

/// Cached state loaded synchronously at boot.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub me: Option<User>,
    pub guilds: Vec<Guild>,
    pub channels: Vec<Channel>,
    pub users: Vec<User>,
    pub read_states: Vec<(ChannelId, ReadState)>,
    pub selected_guild: Option<GuildId>,
    pub selected_channel: Option<ChannelId>,
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

impl Store {
    /// Opens (and migrates) the cache. `None` opens an in-memory database.
    pub fn open(path: Option<&Path>) -> Result<Self> {
        let conn = match path {
            Some(p) => {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir)?;
                    crate::config::restrict_permissions(dir)?;
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Self::migrate(&conn)?;
        if let Some(p) = path {
            // The cache holds message text in plain text: owner-only.
            // SQLite creates -wal/-shm with the database file's mode, but
            // fix them up too in case they predate this build.
            for suffix in ["", "-wal", "-shm"] {
                let f = std::path::PathBuf::from(format!("{}{suffix}", p.display()));
                if f.exists() {
                    crate::config::restrict_permissions(&f)?;
                }
            }
        }
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn migrate(conn: &Connection) -> Result<()> {
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::Store(format!(
                "cache schema v{version} is newer than this build (v{SCHEMA_VERSION})"
            )));
        }
        if version < 1 {
            conn.execute_batch(SCHEMA_V1)?;
            conn.pragma_update(None, "user_version", 1)?;
        }
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| Error::Store("connection mutex poisoned".into()))
    }

    /// Runs `f` on the blocking pool.
    pub async fn blocking<T, F>(&self, f: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Store) -> Result<T> + Send + 'static,
    {
        let this = self.clone();
        tokio::task::spawn_blocking(move || f(&this))
            .await
            .map_err(|_| Error::Cancelled)?
    }

    pub fn schema_version(&self) -> Result<i64> {
        Ok(self
            .lock()?
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }

    // -- meta -------------------------------------------------------------

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        self.lock()?.execute(
            "INSERT INTO meta(key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .lock()?
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    // -- snapshot -----------------------------------------------------------

    pub fn load_snapshot(&self) -> Result<Snapshot> {
        let me = self
            .meta("me")?
            .and_then(|s| serde_json::from_str::<User>(&s).ok());
        let selected_guild = self.meta("selected_guild")?.and_then(|s| s.parse().ok());
        let selected_channel = self.meta("selected_channel")?.and_then(|s| s.parse().ok());
        let conn = self.lock()?;
        let mut guilds = Vec::new();
        {
            let mut st = conn.prepare("SELECT data FROM guilds ORDER BY position, id")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            for r in rows {
                if let Ok(g) = serde_json::from_str(&r?) {
                    guilds.push(g);
                }
            }
        }
        let mut channels = Vec::new();
        {
            let mut st = conn.prepare("SELECT data FROM channels")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            for r in rows {
                if let Ok(c) = serde_json::from_str(&r?) {
                    channels.push(c);
                }
            }
        }
        let mut users = Vec::new();
        {
            let mut st = conn.prepare("SELECT data FROM users")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            for r in rows {
                if let Ok(u) = serde_json::from_str(&r?) {
                    users.push(u);
                }
            }
        }
        let mut read_states = Vec::new();
        {
            let mut st =
                conn.prepare("SELECT channel_id, last_read, mention_count FROM read_states")?;
            let rows = st.query_map([], |r| {
                Ok((
                    sf(r.get(0)?),
                    ReadState {
                        last_read: r.get::<_, Option<i64>>(1)?.map(sf),
                        mention_count: r.get::<_, i64>(2)?.max(0) as u32,
                    },
                ))
            })?;
            for r in rows {
                read_states.push(r?);
            }
        }
        Ok(Snapshot {
            me,
            guilds,
            channels,
            users,
            read_states,
            selected_guild,
            selected_channel,
        })
    }

    /// Replaces the cached guilds/channels/users/read states with READY data.
    /// Messages are kept (they are still valid history).
    pub fn save_ready(&self, ready: &ReadyData, guild_order: &[GuildId]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM guilds", [])?;
        tx.execute("DELETE FROM channels", [])?;
        tx.execute("DELETE FROM read_states", [])?;
        {
            let mut st = tx
                .prepare("INSERT OR REPLACE INTO guilds(id, position, data) VALUES (?1, ?2, ?3)")?;
            for g in &ready.guilds {
                let pos = guild_order
                    .iter()
                    .position(|x| *x == g.id)
                    .unwrap_or(usize::MAX >> 1) as i64;
                st.execute(params![id(g.id), pos, serde_json::to_string(g)?])?;
            }
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO channels(id, guild_id, data) VALUES (?1, ?2, ?3)",
            )?;
            for c in &ready.channels {
                st.execute(params![
                    id(c.id),
                    c.guild_id.map(id),
                    serde_json::to_string(c)?
                ])?;
            }
            let mut st = tx.prepare("INSERT OR REPLACE INTO users(id, data) VALUES (?1, ?2)")?;
            for u in ready.users.iter().chain(ready.me.iter()) {
                st.execute(params![id(u.id), serde_json::to_string(u)?])?;
            }
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO read_states(channel_id, last_read, mention_count) VALUES (?1, ?2, ?3)",
            )?;
            for (c, rs) in &ready.read_states {
                st.execute(params![id(*c), rs.last_read.map(id), rs.mention_count])?;
            }
            if let Some(me) = &ready.me {
                tx.execute(
                    "INSERT INTO meta(key, value) VALUES ('me', ?1)
                     ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                    [serde_json::to_string(me)?],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_guild(&self, g: &Guild, channels: &[Channel]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        tx.execute(
            "INSERT INTO guilds(id, position, data) VALUES (?1, 1000000, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![id(g.id), serde_json::to_string(g)?],
        )?;
        for c in channels {
            tx.execute(
                "INSERT OR REPLACE INTO channels(id, guild_id, data) VALUES (?1, ?2, ?3)",
                params![id(c.id), c.guild_id.map(id), serde_json::to_string(c)?],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn delete_guild(&self, g: GuildId) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM guilds WHERE id = ?1", [id(g)])?;
        conn.execute("DELETE FROM channels WHERE guild_id = ?1", [id(g)])?;
        Ok(())
    }

    pub fn upsert_channel(&self, c: &Channel) -> Result<()> {
        self.lock()?.execute(
            "INSERT OR REPLACE INTO channels(id, guild_id, data) VALUES (?1, ?2, ?3)",
            params![id(c.id), c.guild_id.map(id), serde_json::to_string(c)?],
        )?;
        Ok(())
    }

    pub fn delete_channel(&self, c: ChannelId) -> Result<()> {
        let conn = self.lock()?;
        conn.execute("DELETE FROM channels WHERE id = ?1", [id(c)])?;
        conn.execute("DELETE FROM messages WHERE channel_id = ?1", [id(c)])?;
        Ok(())
    }

    pub fn upsert_users(&self, users: &[User]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare("INSERT OR REPLACE INTO users(id, data) VALUES (?1, ?2)")?;
            for u in users {
                st.execute(params![id(u.id), serde_json::to_string(u)?])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // -- messages -----------------------------------------------------------

    /// Inserts or replaces messages (pending/failed local messages are
    /// never persisted).
    pub fn upsert_messages(&self, msgs: &[Message]) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT OR REPLACE INTO messages(id, channel_id, data) VALUES (?1, ?2, ?3)",
            )?;
            for m in msgs.iter().filter(|m| !m.pending && !m.failed) {
                st.execute(params![
                    id(m.id),
                    id(m.channel_id),
                    serde_json::to_string(m)?
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn patch_message(&self, p: &MessagePatch) -> Result<()> {
        let Some(mut m) = self.message(p.id)? else {
            return Ok(());
        };
        m.apply_patch(p);
        self.upsert_messages(std::slice::from_ref(&m))
    }

    pub fn message(&self, mid: MessageId) -> Result<Option<Message>> {
        let data: Option<String> = self
            .lock()?
            .query_row("SELECT data FROM messages WHERE id = ?1", [id(mid)], |r| {
                r.get(0)
            })
            .optional()?;
        Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
    }

    pub fn delete_messages(&self, ids: &[MessageId]) -> Result<()> {
        let conn = self.lock()?;
        for m in ids {
            conn.execute("DELETE FROM messages WHERE id = ?1", [id(*m)])?;
        }
        Ok(())
    }

    /// Up to `limit` messages of `channel` older than `before` (or the
    /// newest), returned oldest first.
    pub fn messages(
        &self,
        channel: ChannelId,
        before: Option<MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>> {
        let conn = self.lock()?;
        let mut st = conn.prepare(
            "SELECT data FROM messages WHERE channel_id = ?1 AND id < ?2 ORDER BY id DESC LIMIT ?3",
        )?;
        let before = before.map(id).unwrap_or(i64::MAX);
        let rows = st.query_map(params![id(channel), before, limit as i64], |r| {
            r.get::<_, String>(0)
        })?;
        let mut out = Vec::new();
        for r in rows {
            if let Ok(m) = serde_json::from_str::<Message>(&r?) {
                out.push(m);
            }
        }
        out.reverse();
        Ok(out)
    }

    /// Keeps only the newest `keep` messages of a channel.
    pub fn prune_channel(&self, channel: ChannelId, keep: usize) -> Result<usize> {
        let n = self.lock()?.execute(
            "DELETE FROM messages WHERE channel_id = ?1 AND id NOT IN
             (SELECT id FROM messages WHERE channel_id = ?1 ORDER BY id DESC LIMIT ?2)",
            params![id(channel), keep as i64],
        )?;
        Ok(n)
    }

    // -- read states ----------------------------------------------------------

    pub fn set_read_state(&self, channel: ChannelId, rs: ReadState) -> Result<()> {
        self.lock()?.execute(
            "INSERT OR REPLACE INTO read_states(channel_id, last_read, mention_count) VALUES (?1, ?2, ?3)",
            params![id(channel), rs.last_read.map(id), rs.mention_count],
        )?;
        Ok(())
    }

    /// Wipes everything (logout) except user preferences (`pref.*` meta).
    pub fn clear(&self) -> Result<()> {
        self.lock()?.execute_batch(
            "DELETE FROM meta WHERE key NOT LIKE 'pref.%'; DELETE FROM users; DELETE FROM guilds;
             DELETE FROM channels; DELETE FROM messages; DELETE FROM read_states;",
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock;

    fn store() -> Store {
        Store::open(None).unwrap()
    }

    #[test]
    fn migrates_to_current_version() {
        assert_eq!(store().schema_version().unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn file_database_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/cache.sqlite3");
        {
            let s = Store::open(Some(&path)).unwrap();
            s.set_meta("selected_channel", "42").unwrap();
        }
        let s = Store::open(Some(&path)).unwrap();
        assert_eq!(
            s.load_snapshot().unwrap().selected_channel,
            Some(Snowflake(42))
        );
    }

    #[cfg(unix)]
    #[test]
    fn database_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/cache.sqlite3");
        let s = Store::open(Some(&path)).unwrap();
        s.set_meta("k", "v").unwrap();
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        assert_eq!(mode(&path), 0o600);
        let wal = dir.path().join("data/cache.sqlite3-wal");
        if wal.exists() {
            assert_eq!(mode(&wal), 0o600);
        }
    }

    #[test]
    fn ready_roundtrip() {
        let s = store();
        let ready = mock::ready();
        let order: Vec<GuildId> = ready.guilds.iter().rev().map(|g| g.id).collect();
        s.save_ready(&ready, &order).unwrap();
        let snap = s.load_snapshot().unwrap();
        assert_eq!(snap.guilds.len(), ready.guilds.len());
        // Ordered by the given guild order.
        assert_eq!(snap.guilds[0].id, order[0]);
        assert_eq!(snap.channels.len(), ready.channels.len());
        assert_eq!(snap.read_states.len(), ready.read_states.len());
        assert_eq!(snap.me.unwrap().id, ready.me.unwrap().id);
    }

    #[test]
    fn messages_paging_and_prune() {
        let s = store();
        let ch = Snowflake(10);
        let msgs: Vec<Message> = (1..=30)
            .map(|i| mock::text_message(ch, Snowflake(100 + i), mock::user(1), &format!("m{i}")))
            .collect();
        s.upsert_messages(&msgs).unwrap();
        let newest = s.messages(ch, None, 10).unwrap();
        assert_eq!(newest.len(), 10);
        assert_eq!(newest.first().unwrap().id, Snowflake(121));
        assert_eq!(newest.last().unwrap().id, Snowflake(130));
        let older = s.messages(ch, Some(Snowflake(121)), 10).unwrap();
        assert_eq!(older.last().unwrap().id, Snowflake(120));
        assert_eq!(s.prune_channel(ch, 5).unwrap(), 25);
        assert_eq!(s.messages(ch, None, 100).unwrap().len(), 5);
    }

    #[test]
    fn pending_messages_are_not_persisted() {
        let s = store();
        let mut m = mock::text_message(Snowflake(1), Snowflake(2), mock::user(1), "x");
        m.pending = true;
        s.upsert_messages(&[m]).unwrap();
        assert!(s.messages(Snowflake(1), None, 10).unwrap().is_empty());
    }

    #[test]
    fn patch_and_delete() {
        let s = store();
        let m = mock::text_message(Snowflake(1), Snowflake(2), mock::user(1), "before");
        s.upsert_messages(&[m]).unwrap();
        s.patch_message(&MessagePatch {
            id: Snowflake(2),
            channel_id: Snowflake(1),
            content: Some("after".into()),
            edited: true,
            ..Default::default()
        })
        .unwrap();
        let got = s.message(Snowflake(2)).unwrap().unwrap();
        assert_eq!(got.content, "after");
        assert!(got.edited);
        s.delete_messages(&[Snowflake(2)]).unwrap();
        assert!(s.message(Snowflake(2)).unwrap().is_none());
    }

    #[test]
    fn read_state_and_clear() {
        let s = store();
        s.set_read_state(
            Snowflake(5),
            ReadState {
                last_read: Some(Snowflake(9)),
                mention_count: 2,
            },
        )
        .unwrap();
        let snap = s.load_snapshot().unwrap();
        assert_eq!(snap.read_states[0].1.mention_count, 2);
        s.clear().unwrap();
        assert!(s.load_snapshot().unwrap().read_states.is_empty());
    }

    #[tokio::test]
    async fn blocking_runs_off_thread() {
        let s = store();
        let n = s
            .blocking(|s| {
                s.set_meta("k", "v")?;
                s.meta("k")
            })
            .await
            .unwrap();
        assert_eq!(n.as_deref(), Some("v"));
    }

    #[test]
    fn no_credentials_in_schema() {
        let lower = SCHEMA_V1.to_lowercase();
        assert!(!lower.contains("token"));
        assert!(!lower.contains("password"));
    }
}
