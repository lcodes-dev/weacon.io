# Plan 04: a team's name and members

**Status: done** (2026-11-17)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a team's owner can rename the team, invite people to it by email and remove them; an invited person joins through the link in the mail; a person in several teams can switch between them.

## How it works

**Inviting.** The team's owner types an email address. The server saves an invite with a long random token and mails it: "Ada invited you to Ada's team on Weacon", with a "Join the team" button. The button's link is the server's address plus `/invite/` and the token.

**The link and the address.** The token exists only in that mail, and it works only for an account with the invited address, so a forwarded link is useless to anyone else. Nothing is signed with a server key: a random token kept in the database does the same job with no secret to manage.

**Joining.** Opening the link shows "Ada invited you to Ada's team". Someone signed in presses "Join". Someone who isn't signs in or registers first, with the invited address already filled in, then presses "Join". Someone signed in with another address is told which address the invite is for, and can sign out to use it. Joining uses the invite up.

**The server's address.** A new setting, `WEACON_URL`, the address people open in their browser. It defaults to `http://localhost:8080`, and `scripts/dev.sh` sets it to the web app's development address.

**Without SMTP** the invite mail goes to the log like every other mail, link included, so the owner can pass the link on by hand.

**Roles.** A team has one owner, the person who created it, and members. Only the owner renames the team, invites and removes. A member can leave. The owner can't leave or be removed, so every person always has at least their own team.

**Several teams.** A person who joins a second team now has two. The server stops picking one for them: it lists their teams, and every team route names its team in the path. The web app remembers which team is open, in the browser's local storage, and the sidebar gets a team switcher.

## Database

`apps/server/migrations/0002_invites.sql`:

| Table | Columns | Notes |
| --- | --- | --- |
| `invites` | `id`, `team_id`, `email`, `token` (unique), `invited_by` (a user), `expires_at` | One per team and email. It lasts 7 days, or until it is used or revoked |

## API

| Route | Who | Body | Answer |
| --- | --- | --- | --- |
| `GET /api/session` | anyone | | As today, but the user carries `teams` (`id`, `name`, `role`) in place of one team |
| `GET /api/teams/{id}` | a member | | The name, the members (`id`, `name`, `email`, `role`) and the open invites (`id`, `email`) |
| `PUT /api/teams/{id}` | the team's owner | `{"name"}` | 204 |
| `POST /api/teams/{id}/invites` | the team's owner | `{"emails": [...]}` | 204, and each address is mailed. Inviting the same address again makes a new token, starts its 7 days over and sends the mail again; that is what "Resend" does. If one address is wrong or already a member, the request is refused and no mail goes out |
| `DELETE /api/teams/{id}/invites/{invite_id}` | the team's owner | | 204; the link stops working |
| `GET /api/invites/{token}` | anyone | | The team's name, the inviter's name and the invited address; 404 for a token that is used, revoked, expired or unknown |
| `POST /api/invites/{token}/accept` | signed in with the invited address | | 204; they are a member and the invite is gone. 403 for any other account |
| `DELETE /api/teams/{id}/members/{user_id}` | the team's owner, or the member themselves | | 204. Removing and leaving are the same thing. Refused for the owner |

Someone who isn't in the team gets 404 for it, the same as for a team that doesn't exist.

## Server code

```
apps/server/migrations/0002_invites.sql  new
apps/server/src/teams.rs                 new: the team, invite and member routes
apps/server/src/db.rs                    their queries; User carries teams
apps/server/src/mail.rs                  Mail gets an optional button: a label and an address
apps/server/src/lib.rs                   the new routes; the state holds the server's address
apps/server/src/main.rs                  reads WEACON_URL
```

`teams.rs` checks the role with one query, `db::role(team, user)`, at the top of each handler. No new extractor and no new dependency.

**Tests**, in `apps/server/tests/api.rs`:

- The owner renames their team; a member can't.
- The owner invites an address; the invited person registers, joins with the token, and is listed as a member with two teams.
- A known account joins with a token.
- A token works once, and not after the owner revokes the invite.
- Several addresses are invited at once, or none is when one is wrong.
- An expired invite is refused until it is sent again.
- Inviting the same address again replaces the token.
- An unknown token is 404, and joining needs a sign-in.
- An account with another address can't join with the token.
- A member is refused when inviting or removing.
- The owner removes a member; a member leaves; the owner can't leave.
- Someone outside a team gets 404 for it.

The tests read the token from the database, as they read sign-in codes.

## Web app

- **Sidebar.** The team's name at the bottom becomes a switcher listing the person's teams, with "Team settings" beside it.
- **`Invite.tsx`**, shown when the address is `/invite/{token}`: "Ada invited you to Ada's team" and "Join". Signed in with another address, it says "This invite is for lena@example.com" with "Sign out" in place of "Join". Signed out, it sits above the sign-in form with the invited address filled in. After joining, the web app opens that team and goes back to `/`.
- **Pages.** React Router arrives: the team settings are a page at `/settings/team` and the mail settings, until now a dialog, a page at `/settings/mail`, both shown beside the sidebar.
- **`TeamSettings.tsx`**:

```
  Team settings
  Name     [ Ada's team          ] [Save]      owner only

  Members
  Ada Lovelace    ada@example.com     owner
  Marco Rossi     marco@example.com   [Remove]   or [Leave] on your own row

  Invites
  lena@example.com                    [Resend] [Revoke]
  Expires 24/11/2026

  Invite people                                owner only
  [ emails, one per line  ]
  [Send invites]
```

## Steps

- [x] Migration, and the queries in `db.rs`; the session lists teams
- [x] `teams.rs`: the team, invite and member routes
- [x] `WEACON_URL`, and the invite mail with its button
- [x] Server tests
- [x] Web app: the team switcher and the invite screen
- [x] Web app: the team settings page, and the mail settings as a page
- [x] Docs: `docs/architecture.md`, README
- [x] Checked in the browser: rename, invite, join as a second account, switch teams, remove, leave
- [x] Checked by hand: the invite mail in a real mail client

## Outcome

Built as planned, with these differences:

- No `db::role` query: the signed-in user already carries their teams, so `teams.rs` reads the role from there.
- `GET /api/invites/{token}` also says whether the invited address has an account, so the web app opens on "Create an account" or on "Sign in".
- Accepting answers with the team's id, so the web app can open that team.
- The browser check covered rename, invite, joining as a second account with the wrong and the right address, and leaving. Removing a member and switching between two teams were checked by the tests and by the code only.
- After your review on 2026-11-17: both settings became pages, which brought in React Router; invites are typed several at a time, one per line; an invite expires after 7 days and has "Resend" and "Revoke". There is no separate resend route: resending is inviting the same address again.

## Left out on purpose

| Left out | Arrives with |
| --- | --- |
| Creating more teams, and deleting a team | When you ask |
| More than one owner, and handing a team to someone else | When you ask |
| A reusable invite link not tied to a mail, as the reference's "Join a team with an invite link" | When you ask |
| Declining an invite | Not needed: an unused link does nothing |
| A link signed with a server key | A reason to avoid the database lookup |
| Anything that belongs to a team: projects, chats, tasks | Their own plans |
