-- What a content says about itself beyond its symbols, from which the map works out which file uses which.
-- The name its definitions live under, when it declares one: a namespace or a package.
ALTER TABLE blobs ADD COLUMN scope TEXT;

-- What a content says it needs, as written: `App\Models\User`, `./price`.
CREATE TABLE blob_imports (
    project_id TEXT NOT NULL,
    blob TEXT NOT NULL,
    import TEXT NOT NULL,
    PRIMARY KEY (project_id, blob, import),
    FOREIGN KEY (project_id, blob) REFERENCES blobs (project_id, blob) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;

-- A mention now says whether it is written as a member of something else, as `save` is in
-- `user.save`. Every map is read again by the new reader, so nothing is carried over.
DROP TABLE blob_mentions;
CREATE TABLE blob_mentions (
    project_id TEXT NOT NULL,
    blob TEXT NOT NULL,
    name TEXT NOT NULL,
    member INTEGER NOT NULL,
    PRIMARY KEY (project_id, blob, name, member),
    FOREIGN KEY (project_id, blob) REFERENCES blobs (project_id, blob) ON DELETE CASCADE
) STRICT, WITHOUT ROWID;
