CREATE TABLE jobs (
    id TEXT NOT NULL PRIMARY KEY,
    job TEXT NOT NULL,
    unique_key TEXT UNIQUE,
    attempts INTEGER NOT NULL DEFAULT 0,
    run_at INTEGER NOT NULL,
    started_at INTEGER,
    last_error TEXT,
    created_at INTEGER NOT NULL
) STRICT;

DROP TABLE weacon_calls;
