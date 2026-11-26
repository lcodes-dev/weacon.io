-- Whether the SMTP password and the teams' GitHub and API keys are sealed with the server's key.
-- The server seals them at its next start.
ALTER TABLE config ADD COLUMN secrets_sealed INTEGER NOT NULL DEFAULT 0;
