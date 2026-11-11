CREATE TABLE profiles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    model TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE subscriptions (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    sort INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE app_state (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE latency (
    key TEXT PRIMARY KEY,
    delay_ms INTEGER,
    checked_at INTEGER NOT NULL,
    detail TEXT
);

CREATE TABLE mesh_profiles (
    id TEXT PRIMARY KEY,
    payload TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE traffic_sessions (id TEXT PRIMARY KEY, started_at INTEGER NOT NULL);

CREATE TABLE traffic_entities (id TEXT PRIMARY KEY, payload TEXT NOT NULL);

CREATE TABLE traffic_daily (
    session_id TEXT NOT NULL REFERENCES traffic_sessions(id),
    day TEXT NOT NULL,
    core TEXT NOT NULL,
    dimension TEXT NOT NULL,
    entity_id TEXT NOT NULL REFERENCES traffic_entities(id),
    upload INTEGER NOT NULL CHECK(upload >= 0),
    download INTEGER NOT NULL CHECK(download >= 0),
    sampled_at INTEGER NOT NULL,
    has_gaps INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, day, entity_id)
);
CREATE INDEX traffic_daily_range ON traffic_daily(dimension, day);
