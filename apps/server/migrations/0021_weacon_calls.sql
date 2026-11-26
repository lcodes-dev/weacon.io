-- A call to `@weacon` that hasn't answered yet: one at most per group. A row left at startup was cut short.
CREATE TABLE weacon_calls (
    group_id TEXT NOT NULL PRIMARY KEY REFERENCES groups (id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL
) STRICT, WITHOUT ROWID;
