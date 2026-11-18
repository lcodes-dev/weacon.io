-- The run now writing the project's docs. A run that no longer holds it saves nothing.
ALTER TABLE projects ADD COLUMN docs_run INTEGER;

-- A file's language comes from its path, since one content can sit under two names.
ALTER TABLE blobs DROP COLUMN language;
