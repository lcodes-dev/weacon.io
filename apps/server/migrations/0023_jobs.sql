-- Work done after a request was answered, kept until it has run.
CREATE TABLE jobs (
    id TEXT NOT NULL PRIMARY KEY,
    -- JSON with the job's `kind` and its fields.
    job TEXT NOT NULL,
    -- While a job with a key waits or runs, another with the same key isn't queued.
    unique_key TEXT UNIQUE,
    started_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;

-- A call to `@weacon` is a job now, keyed by its group.
DROP TABLE weacon_calls;
