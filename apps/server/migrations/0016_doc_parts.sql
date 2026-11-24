-- A page of the docs cut at its headings, each part with its embedding, to be found by meaning.
CREATE TABLE doc_parts (
    doc_id TEXT NOT NULL REFERENCES docs (id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    text TEXT NOT NULL,
    -- Its numbers, four bytes each, the lowest byte first.
    embedding BLOB NOT NULL,
    PRIMARY KEY (doc_id, position)
) STRICT, WITHOUT ROWID;

-- The version of a page its parts were cut from, and the model that embedded them. A page
-- whose newest version or whose server's model is another has its parts made again.
ALTER TABLE docs ADD COLUMN parts_version TEXT;
ALTER TABLE docs ADD COLUMN parts_model TEXT;
