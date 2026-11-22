# Plan 21: chats

**Status: done** (2026-11-22)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a person opens a chat on a project, picks Claude Code or Codex and one of their machines, and asks for a change. The agent works in a copy of the project's repository on that machine, and what it says and does shows in the web app as it happens. This is the first thing a machine does besides writing docs, and what tasks and connected chats will be built on.

## What a chat is

- **It is one person's, on one project.** It runs on that person's machine, on their own sign-in, under the rule of plan 19. The project's other members can read it; only its owner writes in it.
- **It has one agent and one machine,** picked when it starts and kept. A machine that is away means the chat waits for it.
- **It has a folder of its own on the machine:** a copy of the repository at the branch's newest commit, under the core's folder. The agent may read and change files there and nowhere else.
- **It is a list of turns.** The person writes; the agent answers, and the turn ends. The vendor's own program keeps the conversation, so the next turn goes on where the last one stopped.

## How a turn runs

1. The web app sends the person's message. The server saves it and tells the chat's machine.
2. The core makes the chat's folder if it isn't there. The server gives it a GitHub token for that one repository, good for an hour, with which it fetches; the token is used for that and kept nowhere.
3. The core starts the vendor's unmodified program in that folder, one process a turn: `claude -p --output-format stream-json`, or `codex exec --json`, each told to go on with the chat's earlier conversation.
4. What the program prints is turned into a few kinds of event that are the same for both agents, and sent to the server as it comes: the agent's words, something it did (read a file, edited one, ran a command), and the end of the turn, with the files changed and by how many lines.
5. The server saves each event. The web app asks for the events after the last one it has, every second while a turn runs.

## What the agent may do

The first chats have no "Allow once" yet, so nothing may need a person's answer:

| | Allowed |
| --- | --- |
| Read and edit files in the chat's folder | Yes |
| Run a command | Only inside the vendor's own sandbox, which keeps writes to the folder: Codex's `workspace-write`, Claude Code's sandbox for commands. What would need permission is refused, and the agent is told |
| Reach the network from a command | No |
| Push, or open a pull request | No. The changes stay in the folder, and the chat shows them |

Not yet tried: that each program, run this way, keeps a command inside its sandbox and refuses the rest without asking. The first step tries it on both, and if one can't, that agent's first chats edit files and run nothing.

As in plan 19, a repository's text can mislead the agent. Here that can spoil the chat's folder, and nothing outside it.

## Work outlives the connection

- The core writes each event to a file in the chat's folder before sending it, and numbers it. The server says which number it has; after a drop the core sends what came after.
- A turn goes on while the server or the network is away. It doesn't outlive the core itself: a core that starts and finds a turn unfinished says that it failed.
- "Stop" in the web app ends the program.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/agent-run/` (new) | Runs one turn of either agent in a folder and gives back the common events. The two vendors' formats, their flags and their sandboxes are its business alone. Tested with stand-ins that print recorded output |
| `crates/core-link/` | The new messages: `turn`, `stop`, `event`, `taken` by number |
| `apps/core/` | The chat's folder and its fetch; passes events on and keeps them until taken |
| `apps/server/` | `chats.rs` and its queries; two tables, `chats` and `chat_events`, each read by an index |
| `apps/web/` | "New chat" in the sidebar, the list of chats, and the chat's page: the turns, what the agent did, the files changed, a box to write in, "Stop" |

## Not in this plan

Each is in the prototype and comes after: asking the person for permission; the diff itself, line by line, and notes on a line; a branch, a push and a pull request; "plan" and "build" modes; a chat started from a task; handing part of the work to another agent; commands that keep running, and a preview; showing a sub-agent's own steps. The downloadable core the owner asked for on 22 November is a plan of its own.

## Tests

In `apps/server/tests/api.rs`, with a made-up core as in plan 19: a chat is started and a turn reaches the machine with a token for its repository; events come back in order and are read from a number on; a member reads a chat and can't write in it; someone outside the team sees nothing; a turn on a machine that is away waits; "Stop" reaches the core. In the crate: recorded output of both programs becomes the same events, and a program that ends badly becomes a failed turn.

## Steps

- [x] `crates/agent-run`: one turn of either agent, as common events
- [x] The core: a chat's folder, a turn, events kept until taken
- [x] The server: chats, turns and events
- [x] The web app: starting a chat and its page
- [x] Docs: `docs/architecture.md` and `README.md`
- [x] Checked by hand with both agents on a real machine

## Outcome

Built as planned, on the recommended answer to each of the four questions, with these differences:

- **Both programs keep commands in their sandbox without asking,** tried for real on 22 November: a write outside the folder and a fetch from the network were refused, and the tests ran. Claude Code needs every command allowed for that, which is safe only because none can leave the sandbox. Neither sandbox stops a command from reading other files of the machine, which is written down in the docs and not solved.
- **The server never says which events it has, except when a core connects.** It takes an event only when it is the next one, and on connecting tells the core how many it has. That one message also starts a turn the machine never got and finds one a restarted core lost, so there is nothing to acknowledge and no state to keep in step.
- **"Stop" interrupts the program and doesn't kill it.** Killed, Codex left the command it was running behind; interrupted, both programs end their commands first. Found in the hand check, and now in the crate's tests.
- **None of the person's own setup is loaded,** as in plan 19. With Claude Code that leaves out the repository's own `CLAUDE.md` too, while Codex still reads the repository's `AGENTS.md`.
- **The copy of the repository has no history:** it is fetched at the branch's newest commit only.
- **A chat needs a project with a mapped branch,** and the branch is picked when the chat starts.
- **A chat can't be deleted yet,** and its folder stays on the machine.
- **Tests:** 52 in the server, one new; 4 in the new crate, from what the real programs printed; the core's own chat code has none, and was checked by hand.
- **Checked by hand** on the dev server with a real core and the owner's own sign-ins, on ContentWrite: a Claude Code chat fetched the repository with the one-repository token, edited a file, ran `php -l` in the sandbox and listed the file changed; its second turn went on with the same conversation. A Codex chat did the same while the server was restarted in the middle of its turn, and the page had every event afterwards. "Stop" ended Codex and its command. A core restarted in the middle of a turn said so in the chat. Removing the machine left both chats readable, without a box to write in.
- **Not checked:** a teammate reading a chat in the browser, which the server's test covers; a turn started while the machine is away, likewise; Linux.
