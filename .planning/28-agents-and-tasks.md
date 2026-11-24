# Plan 28: a chat's agent reads and writes tasks

**Status: done** (2026-11-24)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the agent of a chat reads its project's tasks and docs and writes its tasks, as the prototype has it: it ticks a check when it is done, and makes a task of what it found when asked to. The owner asked for this on 24 November. It needs plan 27.

## What an agent can do

A chat's agent gets tools, for its chat's project only:

| Tool | What it does |
| --- | --- |
| `tasks` | Lists the project's tasks: name, title, status, owner |
| `task` | Reads one by its name, such as `SF-12`, or the chat's own when none is named: text, checks and the docs it touches |
| `new_task` | Makes one, from a title and optionally a text and checks |
| `change_task` | Changes a task's title, text, checks or status, and ticks a check |
| `doc` | Reads a page of the project's docs by its title |

- **What an agent writes takes effect at once,** as what a member writes does, and is the chat's owner's doing: a task it makes was asked by that person. The task says which chat it came from, and the chat shows a line for each task made or changed, with a link.
- **An agent doesn't delete a task,** give one to somebody, or change the docs. A change to the docs stays a proposal, which is a later plan.
- **A chat started from a task** is told in its first message to read that task, so the agent has the checks and the pages as they are then, not as they were pasted.

## How an agent gets the tools

Both programs can be given a tool server when they are started. Tried on 24 November with a stand-in server of two tools, each program started as Weacon starts it:

- **Codex** took the server from two settings on its command line, and called both tools once a third said its calls need no asking. Without that one it refused them, since nobody can be asked in a chat.
- **Claude Code** takes one from `--mcp-config`. But the flag Weacon starts it with to leave the person's setup out, `--safe-mode`, turns every tool server off, this one included. Started instead with no settings loaded, `--setting-sources ""`, it called both tools; a hook in the repository's settings did not run, the repository's own tool server was not started, and its `CLAUDE.md` was not loaded.

So Claude Code is started another way, and what that lets in has to be known first: the first step. The machine it was tried on has no skills, commands or agents of the person's own, so whether those load is not known yet.

**The server is the tool server.** It answers the tools at an address of its own, and each program reaches it over HTTPS with a token. Nothing new runs on the machine.

- **The token** is made by the server for one turn of one chat, sent to the core with the turn and kept nowhere: the server keeps its hash on the chat's row and forgets it when the turn ends. It opens the tools for that chat's project and nothing else.
- **The agent's program makes the call, not a command of its,** so the sandbox and a chat's hosts have no part in it.
- **Not tried yet:** both programs with a server reached over HTTPS; only one started on the machine was. That is the second step. If one of them can't, the core's program gets a mode that speaks to the agent and passes each call on to the server with the same token.

The same address can later serve other programs a person uses, as the prototype's settings show, with a token of the person's. That is not in this plan.

## Where the code goes

| Where | What |
| --- | --- |
| `apps/server/src/tools.rs` | The address the agents call: the list of tools and each tool, checked against the turn's token |
| `apps/server/src/db.rs` and a migration | The token's hash and its chat; where a task came from |
| `apps/server/src/chats.rs` | A turn is sent with its token; a tool that writes adds a line to the chat |
| `crates/core-link/` | The token and the address in a turn |
| `crates/agent-run/` | Each program started with the tool server; Claude Code without `--safe-mode` |
| `apps/web/src/components/ChatEvents.tsx`, `ProjectTasks.tsx` | The line in a chat; the chat a task came from |
| `docs/architecture.md` | What an agent can do with tasks and docs, and how it is let in |

## Tests

- In `apps/server/tests/api.rs`: with a turn's token each tool does what it says, for the chat's project; a task of another project isn't found; a token of a turn that ended, and no token, are refused; a task made by an agent names its chat and its owner, and the chat has the line.
- In `crates/agent-run/tests/`: each program is started with the tool server's address and token.
- That the real programs call the tools can only be checked by hand.

## Steps

- [x] Find what Claude Code loads of a person's own when started with no settings in place of `--safe-mode`, and stop if it is more than Weacon can leave out
- [x] Try both programs against a tool server reached over HTTPS, and fall back to the core passing calls on if one can't
- [x] Server: the tools and the turn's token, with their tests
- [x] `core-link`, the core and `agent-run`: a turn carries the token, and each program is started with the tool server
- [x] Web app: the line in a chat, and the chat on a task
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand with both agents on ContentWrite: a chat from a task reads it, ticks a check, and makes a task of something it found

## Outcome

Built as planned, with these differences:

- **A turn's token is kept as it is, not as a hash.** The server sends a running turn again to a core that connects, and has to send the same token. It is replaced when the next turn starts and opens nothing once its turn has ended.
- **The token reaches each program in its environment,** as `WEACON_TOOLS`, and is in no command line: Claude Code fills it into the header itself, and Codex is given the variable's name.
- **The server sends the tools' address with the turn,** from the address people open it at.
- **A check is ticked by sending the task's whole list of checks,** each with whether it is done. There is no tool for one check.
- **Calls to the tools aren't shown in a chat;** what they wrote is, as a line the server adds.

What the first two steps found, on 24 November:

- **Claude Code started with no settings in place of `--safe-mode`** loaded nothing of a repository's own: no instructions, skills, commands, agents, hook or tool server. In the person's default folder, which has skills and instructions of their own, it loaded none of those either. Its built-in skills and commands are there in both ways of starting.
- **Both programs called a stand-in tool server over HTTP** with the token, so the core needs no mode of its own.

- **Tests:** 58 on the server, one of them new: an agent's tools, with and without the token, during and after its turn. In `agent-run`, each program is started with the tool server and gets the token in its environment.
- **Checked by hand through Weacon** on ContentWrite, with the real programs. Claude Code, started from CON-5, read the task, wrote the file, ticked both checks, set the task to done and made CON-6, which says which chat it came from. Codex, started from CON-6, read the task and a page of the docs, added a check keeping the others, and set it to doing. Each chat shows a line for what was made or changed.
- **Not checked:** a person's own commands and agents with Claude Code, since this machine has none; a server reached over HTTPS and not through the dev proxy.
