-- A session lasts from its last renewal, not from the sign-in.
ALTER TABLE sessions RENAME COLUMN created_at TO renewed_at;
