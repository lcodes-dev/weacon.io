/// Sign-in codes as they are, not hashed; the live ones are dropped, so their people ask again.
pub(crate) const PLAIN_CODES: &str = "
DROP TABLE sign_in_challenges;

CREATE TABLE sign_in_challenges (
    challenge_id TEXT PRIMARY KEY,
    email TEXT NOT NULL,
    code TEXT NOT NULL,
    expires_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL,
    mailed INTEGER NOT NULL
) STRICT;

CREATE INDEX sign_in_challenges_email ON sign_in_challenges (email);
";
