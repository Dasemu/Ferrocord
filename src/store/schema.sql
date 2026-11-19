-- Ferrocord local cache. Version 1.
-- This database is a cache: it can be deleted at any time and is rebuilt
-- from the network. It never contains credentials.

CREATE TABLE IF NOT EXISTS meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
    id   INTEGER PRIMARY KEY,
    data TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS guilds (
    id       INTEGER PRIMARY KEY,
    position INTEGER NOT NULL DEFAULT 0,
    data     TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS channels (
    id       INTEGER PRIMARY KEY,
    guild_id INTEGER,
    data     TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS channels_by_guild ON channels(guild_id);

CREATE TABLE IF NOT EXISTS messages (
    id         INTEGER PRIMARY KEY,
    channel_id INTEGER NOT NULL,
    data       TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_by_channel ON messages(channel_id, id);

CREATE TABLE IF NOT EXISTS read_states (
    channel_id    INTEGER PRIMARY KEY,
    last_read     INTEGER,
    mention_count INTEGER NOT NULL DEFAULT 0
);
