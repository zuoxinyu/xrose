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

CREATE TABLE IF NOT EXISTS health_probe_samples (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    target_key TEXT NOT NULL,
    kind TEXT NOT NULL,
    ts INTEGER NOT NULL,
    ok INTEGER NOT NULL,
    rtt_ms INTEGER,
    bps INTEGER,
    bytes_used INTEGER NOT NULL DEFAULT 0,
    err_stage TEXT NOT NULL DEFAULT 'none'
);
CREATE INDEX IF NOT EXISTS health_probe_samples_target ON health_probe_samples(target_key, ts DESC, id DESC);

CREATE TABLE IF NOT EXISTS health_reachability_cache (
    egress_hash TEXT PRIMARY KEY,
    ts INTEGER NOT NULL,
    payload TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS health_target_reachability (
    target_key TEXT PRIMARY KEY,
    egress_hash TEXT NOT NULL,
    udp_status TEXT NOT NULL DEFAULT 'untested',
    updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS health_hourly_buckets (
    target_key TEXT NOT NULL,
    hour_bucket INTEGER NOT NULL,
    ok_weighted REAL NOT NULL,
    total_weighted REAL NOT NULL,
    rtt_p50_ms INTEGER,
    rtt_p90_ms INTEGER,
    tput_p75_bps INTEGER,
    PRIMARY KEY (target_key, hour_bucket)
);
CREATE INDEX IF NOT EXISTS health_hourly_buckets_time ON health_hourly_buckets(hour_bucket);
