# Plan 33: `@weacon`, the team's own agent

**Status: done** (2026-11-25)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: in a group, anybody writes `@weacon` and the team's agent answers there from the project's docs and tasks, on the team's API key, with nobody's machine. The prototype shows it as "@weacon · team", "Team AI for docs and tasks, set up by an owner". It is the shared half of the rule of 20 November: a subscription is a person's, the API key is the team's. It needs plan 32.

## What it is

- **It runs on the server,** which calls the Anthropic API with the team's key. Nothing runs on a machine, so it answers when everybody's laptop is closed.
- **It has no repository.** It runs no command and changes no file. It has the six tools a chat's agent has for its project: `tasks`, `task`, `new_task`, `change_task`, `doc` and `search_docs`. Code is reached through the docs, which name the files they were written from.
- **It works for who tagged it.** A task it makes was asked by that person. The others' messages are what they said, never instructions, as in plan 32.
- **It remembers nothing of its own.** Each call it is told the group's last fifty messages, its own earlier answers among them, each with who wrote it. So it keeps no chat, and what it knows is what the group can read.
- **Its answer is a message in the group,** of the agent's kind, named "Weacon". While it works the group says so. A call that fails posts why.

## When a tag can't be followed

As for the other agents, the message is posted and its writer is told why nothing runs:

- the team has no API key;
- the group has no project;
- it is still working on the last call in this group.

## The team's key

Today the key is kept only while the docs are written by the API: choosing Claude Code or Codex for the docs forgets it. That changes:

- **The key is the team's,** kept whoever writes the docs, and set or removed by the owner in the team's settings in a row of its own. The docs' choice then only says who writes; "the API" can be chosen once there is a key.
- **One model for both,** the one the team's settings already have, with today's default.

## How a call runs

1. The group's message is written, and the tag found beside `@claude` and `@codex`.
2. The server starts the work and answers the request at once; the page learns the rest by asking again, as it does today.
3. The server sends the model who it works for, the group's messages and the tools. While the model asks for a tool, the server runs it, with the same code a chat's agent reaches, and sends the result back: at most ten rounds.
4. The model's last words are posted to the group.

Which groups it is working in is kept in the server's memory, not in the database: a restart loses the call itself, so nothing can stay "working" for good.

## Where the code goes

| Where | What |
| --- | --- |
| `apps/server/src/weacon.rs` | A call: what the model is told, the rounds of tools, and the answer posted |
| `apps/server/src/tools.rs` | A tool is run for a project and a person, with a chat or without; a chat's line is added only when there is one |
| `apps/server/src/writer.rs` | The key kept apart from who writes the docs; the request to Anthropic shared by both |
| `apps/server/src/groups.rs` | The tag, why it isn't followed, and "working" |
| `apps/web/src/routes/Group.tsx`, the team's settings | Weacon's answers and working line, `@weacon` in the hint under the box; the key's own row |
| `docs/architecture.md` | The team's agent, and the key |

No migration: an answer is a row of `group_messages` with `weacon` as its agent and no chat, and the key stays where it is. No crate: the rounds of tools are one loop.

## Where this differs from the project's spec and the prototype

- **In groups only.** There is no chat with `@weacon` alone; a group of one person does that.
- **It doesn't search the web or read the code itself.**
- **Nothing counts what a team has spent.** The key's own vendor shows that.

## Not in this plan

- A proposal to change the docs, by any agent.
- `@weacon` speaking without being tagged.
- Another vendor's key.
- A limit on how often a member calls it.

## What leaves the server

A call sends the group's last fifty messages, and whatever tasks and docs the model reads, to Anthropic under the team's key. The docs already go there when the API writes them; what people wrote to each other is new.

## Tests

In `apps/server/tests/api.rs`, with the stand-in for Anthropic the docs' tests use:

- `@weacon` in a group with a project and a team's key sends the group's messages with their writers, and the tools; its answer is posted in the group as Weacon's.
- When the model asks for a tool, the tool is run for the group's project and its result sent back; a task it makes was asked by who tagged it.
- A tag is not followed, and says why, without a key, without a project, and while a call runs.
- Anthropic refusing, and more rounds than allowed, post why in the group.
- The key is kept when the docs' writer is changed to an agent on a machine, and only the owner sets or removes it.

That the real model answers well in a group is checked by hand.

## Steps

- [x] Server: the team's key apart from the docs' writer, with its test
- [x] Server: a tool run without a chat
- [x] Server: a call, its rounds of tools and its answer in the group, with their tests
- [x] Web app: Weacon in a group, and the key in the team's settings
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand with a real key on ContentWrite: a question answered from the docs, a task made when asked, and a second call that knows the first

## Outcome

Built as planned, with these differences:

- **The key has requests of its own,** `PUT` and `DELETE /teams/{id}/key`, and the docs' writer's request no longer takes one. The key can't be removed while the API writes the docs.
- **The model is the docs' only while the API writes them.** With Claude Code or Codex as the writer, the model in the settings is that program's, so `@weacon` then uses the cheap default.
- **A group's page asks who is working before it asks for the messages,** so that an answer posted between the two is never missed. It was the other way round, for the other agents too.
- **A person's agent is told Weacon's answers as "Weacon, the team's agent",** not as that person's words.

- **Tests:** 63 on the server, one of them new, which covers the plan's list; the docs' writer's test now saves the key apart.
- **Checked by hand** in the dev app: `@weacon` in a group of a team without a key is written, set apart, and says why nothing runs; the key's row in the team's settings saves and removes a made-up key, the API can be chosen for the docs only with one, and the line under a group's box names `@weacon` and what it sends once there is one.
- **Checked by the owner** with the team's real key: `@weacon` answers in a group.
