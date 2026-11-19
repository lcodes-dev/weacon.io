-- The files a page was written from, as a JSON list, so that the writer can read them again.
-- Empty for a page a person made, until the writer has said which files it is about.
ALTER TABLE docs ADD COLUMN files TEXT NOT NULL DEFAULT '[]';
-- Where the writer finds a person's words in the page at odds with the code, as a JSON list.
-- NULL when it hasn't checked the page as it is now.
ALTER TABLE docs ADD COLUMN notes TEXT;
-- Whether the writer is checking the page after a person's edit.
ALTER TABLE docs ADD COLUMN checking INTEGER NOT NULL DEFAULT 0;

-- A change to a project's docs that the writer suggests, and that changes nothing until a person accepts it.
CREATE TABLE proposals (
    id TEXT NOT NULL PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects (id) ON DELETE CASCADE,
    -- The page to change, or to remove when there is no markdown. NULL for a new page.
    doc_id TEXT REFERENCES docs (id) ON DELETE CASCADE,
    -- The page's version the proposal was written against.
    base TEXT,
    section TEXT NOT NULL,
    title TEXT NOT NULL,
    markdown TEXT,
    -- JSON lists: the files the text was written from, and the writer's notes on a person's words.
    files TEXT NOT NULL,
    notes TEXT NOT NULL,
    -- Why, in a sentence.
    reason TEXT NOT NULL,
    -- What a person asked for, when the proposal answers a request.
    request TEXT,
    created_at INTEGER NOT NULL
) STRICT;

CREATE INDEX proposals_project ON proposals (project_id, id);
-- For the cascade from a deleted page.
CREATE INDEX proposals_doc ON proposals (doc_id);
