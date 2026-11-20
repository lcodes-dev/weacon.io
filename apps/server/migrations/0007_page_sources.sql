-- What a page was written from is kept beside its text, no longer in it: the commit, and the
-- files the writer was given but had no room to read. The files it did read are in `files`.
ALTER TABLE docs ADD COLUMN commit_sha TEXT;
ALTER TABLE docs ADD COLUMN unread TEXT NOT NULL DEFAULT '[]';
ALTER TABLE proposals ADD COLUMN commit_sha TEXT;
ALTER TABLE proposals ADD COLUMN unread TEXT NOT NULL DEFAULT '[]';

UPDATE docs SET commit_sha = (SELECT docs_commit FROM projects WHERE id = docs.project_id);

-- A run used to end a page with these two lines. Each is taken off where it is the text's
-- last line, the later one first; a text that only holds such words further up is left whole.
UPDATE docs SET markdown = substr(markdown, 1, instr(markdown, char(10, 10) || 'Not read, for lack of room: ') - 1)
WHERE instr(markdown, char(10, 10) || 'Not read, for lack of room: ') > 0
  AND instr(substr(markdown, instr(markdown, char(10, 10) || 'Not read, for lack of room: ') + 2), char(10)) = 0;
UPDATE docs SET markdown = substr(markdown, 1, instr(markdown, char(10, 10) || 'Written from: ') - 1)
WHERE instr(markdown, char(10, 10) || 'Written from: ') > 0
  AND instr(substr(markdown, instr(markdown, char(10, 10) || 'Written from: ') + 2), char(10)) = 0;
UPDATE doc_versions SET markdown = substr(markdown, 1, instr(markdown, char(10, 10) || 'Not read, for lack of room: ') - 1)
WHERE instr(markdown, char(10, 10) || 'Not read, for lack of room: ') > 0
  AND instr(substr(markdown, instr(markdown, char(10, 10) || 'Not read, for lack of room: ') + 2), char(10)) = 0;
UPDATE doc_versions SET markdown = substr(markdown, 1, instr(markdown, char(10, 10) || 'Written from: ') - 1)
WHERE instr(markdown, char(10, 10) || 'Written from: ') > 0
  AND instr(substr(markdown, instr(markdown, char(10, 10) || 'Written from: ') + 2), char(10)) = 0;
