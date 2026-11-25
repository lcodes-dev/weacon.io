# Plan 30: deleting a chat

**Status: done** (2026-11-25)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a chat can be deleted, and its folder on the machine goes with it. Today a chat stays for good, and so do its copy of the repository and its cache. The owner asked for it on 25 November, to be built at once.

## What deleting does

- **Who:** the chat's owner, and nobody else.
- **When:** not while a turn runs; it is stopped first.
- **On the server:** the chat and its events go. A task the chat was for stays, and so does a task its agent made, each without the chat.
- **On the machine:** the chat's folder goes: its copy of the repository, git's folder for it, its cache and its events. The server asks the core and deletes its own rows only once the core has answered, so a folder is never left behind without anybody knowing.
- **A machine that is away** can't be asked, so the chat can't be deleted until it is back. A chat whose machine was removed has no folder to reach, and is deleted on the server alone.
- **On GitHub nothing changes:** a branch the chat pushed and its pull request are the work's result, and stay.

Nothing asks twice, as with a project or a task. What the chat changed and never pushed is lost with its folder.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/core-link/` | `forget`, which the server sends for a chat and the core answers like an ask |
| `apps/core/src/chats.rs`, `link.rs` | Removing the chat's folder |
| `apps/server/src/chats.rs`, `db.rs`, `lib.rs` | `DELETE /api/teams/{id}/chats/{chat_id}` |
| `apps/web/src/routes/Chat.tsx` | "Delete chat" in the chat's header, for its owner |
| `docs/architecture.md` | What deleting a chat does |

## Tests

In `apps/server/tests/api.rs`: only the owner deletes; not while a turn runs; the core is asked and the chat goes once it has answered, and stays when the core says it couldn't; a chat whose machine is away stays, and one whose machine was removed goes; a task the chat was for, and one its agent made, stay without it.

## Steps

- [x] `core-link` and the core: forgetting a chat's folder
- [x] Server: the request, with its tests
- [x] Web app: "Delete chat"
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand: a chat deleted in the browser is gone, and so is its folder on the machine

## Outcome

Built as planned.

- **Tests:** 60 on the server, one of them new.
- **Checked by hand** in the browser: "Delete chat" on a chat of this machine removed the chat, and its folder was gone from the core's `chats/`.
- **Not checked by hand:** a chat whose machine is away or was removed, which the test covers.
