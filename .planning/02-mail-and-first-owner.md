# Plan 02: mail, accounts and the first owner

**Status: in progress**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the server can send mail, anyone can register and sign in with their email, and the first person to register on a fresh server becomes its owner, who can then set up SMTP.

## How it works

**Signing in.** There are no passwords. A person types their email, the server mails them a 6-digit code, and they type the code.

**Registering.** Anyone can register: name and email, then the code. Registering creates the account and a team of their own, named after them ("Ada's team"), as Laravel's starter kits do. Being invited to someone else's team is a later plan.

**The first owner.** A fresh server has no accounts, so the web app opens on "Set up Weacon", which is the register form under another title. Whoever registers first is the server's owner. Nothing else guards it: an open server belongs to whoever gets there first.

**The owner** is the owner of the server, not of a team. For now the one thing only they can do is change the mail settings.

**Mail before SMTP exists.** A fresh server has no SMTP settings, so it prints every mail to its log, codes included. The first owner reads their code there, signs in, and enters the SMTP settings. Until they do, the owner sees a notice, "Mail isn't set up. Sign-in codes are only printed to the server's log.", and only someone who can read the log can register or sign in.

## Choices

- **Setup:** no separate setup link or token. The sign-in code in the log is the one mechanism that proves someone can reach the server.
- **Settings from the environment:** SMTP is set in one place, the web app. No environment variables that override it.
- **Mail settings:** one row in a `config` table (host, port, security, username, password, sender) that the owner edits, with a "send a test mail" button.
- **Kept simple:** the SMTP password is stored as it is, the settings are cached in memory, and registration is open.

## Mail

One file, `mail.rs`, with one function: `send(to, subject, text)`. Plain text only.

| SMTP settings in the database | What `send` does |
| --- | --- |
| None | Prints the mail to the server's log: recipient, subject and text |
| Present | Sends it over SMTP |

The settings are read once at startup and kept in memory as a ready SMTP connection. Saving new settings replaces what is in memory, so nothing is read from the database per mail and no restart is needed.

New dependency: `lettre`, the usual Rust SMTP client.

## Database

This is the first table, so migrations arrive now: `apps/server/migrations/0001_accounts.sql`, embedded and run at startup.

| Table | Columns | Notes |
| --- | --- | --- |
| `users` | `id`, `email` (unique), `name`, `is_owner`, `created_at` | `is_owner` marks the server's owner |
| `teams` | `id`, `name`, `created_at` | |
| `team_members` | `team_id`, `user_id`, `role` (`owner` or `member`) | The role is within the team |
| `sign_in_codes` | `email` (primary key), `code`, `name`, `expires_at`, `attempts` | One live code per email; a new request replaces it. `name` is set when the request was a registration |
| `sessions` | `token` (primary key), `user_id`, `created_at` | |
| `config` | `id` (always 1), `smtp_host`, `smtp_port`, `smtp_security` (`tls`, `starttls` or `none`), `smtp_username`, `smtp_password`, `mail_from` | One row; every column empty until the owner saves |

Times are whole seconds since 1970. Emails are trimmed and lowercased before they are stored or compared.

## API

| Route | Who | Body | Answer |
| --- | --- | --- | --- |
| `GET /api/session` | anyone | | `{"setup_needed": bool, "user": {...} or null}`, the user with their team. The one call the web app makes to decide what to show |
| `POST /api/sign-in/code` | anyone | `{"email", "name"?}` | Always 204. With a name it is a registration and always mails a code; without, it mails one only to a known account |
| `POST /api/sign-in/verify` | anyone | `{"email", "code"}` | The user, and a session cookie. A registration creates the account and its team here; the first one is the owner |
| `POST /api/sign-out` | signed in | | 204, and the session is deleted |
| `GET /api/config/mail` | owner | | The SMTP settings, without the password |
| `PUT /api/config/mail` | owner | The SMTP settings | 204; saved, and in use from the next mail |
| `DELETE /api/config/mail` | owner | | 204; mail goes back to the log |
| `POST /api/config/mail/test` | owner | | Sends a test mail to the owner, and answers with the SMTP error if it fails |

- **Codes** last 10 minutes and die after 5 wrong tries. Without that limit a 6-digit code can be guessed, so this is the one defence in the plan.
- **The session cookie** is `HttpOnly` and `SameSite=Lax`, read and written by hand: no cookie library.
- **Errors** become one small type that answers `{"error": "..."}` with a status code.

## Server code

```
apps/server/
  migrations/0001_accounts.sql
  src/main.rs    settings, startup, the router (grows a little)
  src/db.rs      opens the database, runs migrations, and holds every query
  src/mail.rs    new: send, to the log or over SMTP, and the cached settings
  src/auth.rs    new: the session and sign-in routes, and reading the signed-in user from the cookie
  src/config.rs  new: the mail settings routes
```

The router's state becomes a small struct holding the database pool and the mailer, now that there are two things to hold.

New dependencies (2): `lettre` for SMTP and `rand` for codes and session tokens.

**Tests:**

- A fresh server says setup is needed.
- The first person to register becomes the owner, gets a team, and is signed in.
- A second person can register, gets their own team, and is not the owner.
- A known account can sign in again, and sign out.
- An unknown email asking to sign in gets no code.
- A wrong code is refused, and the fifth wrong try kills the code.
- An expired code is refused.
- Only the owner can read or change the mail settings.
- Saving the mail settings switches mail to SMTP, and deleting them switches it back to the log.

Tests read the code from the database, so mail needs no fake. No test sends over a real SMTP server; the test mail button is how that is checked.

## Web app

Before the shell, `main.tsx` asks `GET /api/session` and shows one of three things:

| Server says | The web app shows |
| --- | --- |
| Setup needed | "Set up Weacon": name and email, then the code |
| No user | "Sign in": email, then the code, with a link to "Create an account" (name and email) |
| A user | The shell, with their name and team at the bottom of the sidebar, and "Sign out". The owner also sees the notice about mail until SMTP is saved |

Two new files:

- `SignIn.tsx`: the register and sign-in forms and the code step.
- `MailSettings.tsx`: a dialog only the owner can open from the sidebar, with the SMTP fields, "Save", "Send a test mail" and "Stop using SMTP".

Both use UI SDK (field, input, one-time-code input, button, dialog), the first real use of the library. No router yet: this is still one screen, with one dialog over it.

## Steps

- [x] Migration and the queries in `db.rs`
- [x] `mail.rs`: the log, SMTP, and the cached settings
- [x] `auth.rs`: session, register, sign in, sign out
- [x] `config.rs`: the mail settings routes
- [x] Server tests
- [x] Web app: setup, register and sign-in screens
- [x] Web app: the mail settings dialog
- [x] Docs: `docs/architecture.md`, README
- [x] Checked in the browser: set up the owner on a fresh database with the code from the log, see the mail notice, sign out, sign in again, register a second account
- [ ] Checked by hand: a test mail over a real SMTP server

## Outcome

Built as planned, with these differences:

- During setup the code step says the code is in the server's log, since no SMTP can be set yet.
- The mail settings dialog stays open after saving, so "Send a test mail" is one click away.
- `serde_json` moved from a test dependency to a real one, so the small JSON answers are written inline instead of as one struct each.
- The last step is open: no mail has gone through a real SMTP server yet. A test mail to an unreachable server shows the connection error as intended.

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| Inviting people to a team, and switching between teams | The plan for teams |
| Projects inside a team | Its own plan |
| Renaming a team, or changing your name or email | Settings |
| HTML mail and templates | The first mail that needs them |
| A switch to close registration | When you ask |
| Encrypting the SMTP password, limits on code requests, session expiry | See improvements |
