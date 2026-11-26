# Plan 39: sealed secrets

**Status: done** (2026-11-26)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a copy of the database alone doesn't give away the three secrets it holds: the SMTP password, each team's GitHub App private key and each team's API key. Today they are plain text, so anyone who reads the file, or a backup of it, can send mail as the server, act as a team's app on GitHub, or spend on a team's Anthropic account. That matters once the server runs somewhere someone else hosts. Sessions, machines' tokens and turns' tokens are left as they are: they are Weacon's own, and their item in `improvements.md` is to keep a hash of them, not a sealed copy.

## The key

- **One key for the server:** 32 random bytes in a file, `weacon.key` beside the database by default, or wherever `WEACON_KEY_FILE` says, so that a host can keep it on another disk or in its secrets mount, away from the database and its backups.
- **Made on the first start,** readable by the server's account only, like the database. It is made only when the database holds no sealed secret yet. With sealed secrets and no key file, the server refuses to start and says which file it looked for, instead of making a new key that can't open them.
- **The wrong key stops the start too:** at startup the server opens every sealed secret once, and a file that opens none of them is named in the error. So a missing or mixed-up key shows at once, not at the next mail.
- **Losing the key loses only the three secrets.** The owner types the SMTP password and the team's key again and registers the GitHub App again; nothing else in the database depends on it. `docs/architecture.md` says to back the key up apart from the database.

## Sealing

- **AES-256-GCM from `aws-lc-rs`,** which the build already holds through `jsonwebtoken` and `rustls`; the server names it as a dependency of its own, and no new crate is downloaded. A random 12-byte nonce for each value, which is safe for the few thousand writes a server makes.
- **Stored in the same `TEXT` columns** as base64 of the nonce and the sealed bytes (`base64` is in the build too). No column changes type, so no table is rebuilt.
- **Each value is bound to its place:** the column's name and, for a team's, the team's id go in as associated data. A sealed value copied into another team's row, or from one column to another, doesn't open.
- **Sealed and opened in `db.rs`,** in the four functions that save and read them (`save_smtp`/`smtp`, `save_github_app`/`github_app`, `save_api_key`/`api_key`, and `writer`, which reads the team's key too). They take the key from `AppState`, so the code that sends mail, calls GitHub or the API doesn't change.
- **A small module, `secrets.rs`,** holds the key: `load`, which makes or reads it, seals what is still plain and checks the rest, and `seal` and `open`. It is about fifty lines and has no rules beyond these, so it stays in the server rather than in `crates/`.

## Databases that already hold plain secrets

A migration adds `config.secrets_sealed`, `0` on an existing database. At startup, when it is `0`, the server seals every secret it finds, in one transaction that ends by setting it to `1`. A new database starts at `0` too, and has nothing to seal. The step runs once per database; after it, every secret is sealed and no code reads plain ones.

## Decided

- **The key lives in a file,** not in an environment variable, which leaks more easily into a log or a crash report. *(asked of the owner, 2026-11-26)*
- **A key that can't open the secrets stops the start,** so a lost key shows at once. *(asked of the owner, 2026-11-26)*

## Tests

In `apps/server/tests/`, through the API, with the key loaded as `main.rs` loads it:

- The SMTP password, the GitHub App's key and the team's API key, saved through their routes, are not in the database as text, and still work: `db::smtp` reads the password back as it was typed, a GitHub list signs its token for the fake GitHub, and an `@weacon` call sends the key to the fake API.
- A database with plain secrets, written as an old server wrote them, is sealed by the first start and still works.
- A start with sealed secrets and no key file fails and makes no file; a start with another key fails.
- A team's sealed key copied into another team's row doesn't open.

## Where it goes

| Where | What |
| --- | --- |
| `apps/server/src/secrets.rs` | The key, `seal` and `open` |
| `apps/server/src/db.rs` | The four functions, and the one-time sealing |
| `apps/server/src/main.rs`, `lib.rs` | `WEACON_KEY_FILE`, the key in `AppState` |
| `apps/server/migrations/0021_secrets_sealed.sql` | `config.secrets_sealed` |
| `.sqlx/` | The changed queries |
| `apps/server/tests/api.rs` | The tests above |
| `docs/architecture.md` | A "Secrets" section, and the key beside the database file |
| `.planning/improvements.md` | The three items removed |
| `README.md` | `WEACON_KEY_FILE` among the settings |
| `.planning/open_questions.md` | Question 8 removed |

## Not in this plan

- Changing the key. A new key would mean sealing every secret again; until someone needs it, the owner can type them again.
- A key held by an outside service, such as a cloud's key manager.
- Hashing sessions', machines' and turns' tokens, which is its own item in `improvements.md`.
- Encrypting the rest of the database, such as source code and chats.

## Steps

- [x] `secrets.rs`: the key file, made or loaded, `seal` and `open`
- [x] The migration and the one-time sealing at startup, with the checks for a missing or wrong key
- [x] The four functions in `db.rs` seal and open; `.sqlx/` prepared
- [x] The tests
- [x] `docs/architecture.md`, `improvements.md` and `open_questions.md`
- [x] `scripts/check.sh` passes

## Outcome

Built as planned. `scripts/check.sh` couldn't run as written here, since mise couldn't be fetched; its steps ran one by one with the pinned Rust. Format, clippy and every test passed, except the docs search's, which couldn't fetch its model on this network and which this plan doesn't touch.
