# Plan 23: hosts a chat may reach

**Status: done** (2026-11-24; rewritten on 23 November, after the first version stopped at its first step)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: an agent's commands can reach the network hosts the chat's owner allows, and no others. Today they reach none, so an agent can't install a package or run a test suite that fetches anything, and is refused without a word to the person.

The first version of this plan had each agent ask to be let through, and Codex leave its sandbox on a yes. It stopped because Codex can only ask when run in a way that loads the person's own Codex settings. What was found is kept at the end.

## What a chat gets

- **A list of hosts,** such as `registry.npmjs.org`, empty at the start. The commands of the chat's agent reach those and nothing else. Both programs are started with the list on every turn.
- **Neither agent ever leaves its sandbox.** An allowed host changes what a command can reach, not where it can write.
- **Only the chat's owner allows a host** or takes one off the list. Teammates see the list and what was asked.

## How a new host is allowed

The two programs differ in when the person can say yes:

| | Claude Code | Codex |
| --- | --- | --- |
| A command wants a host that isn't on the list | Claude Code asks and waits | Codex's proxy refuses, and the command fails |
| What the chat shows | The host, with "Allow in this chat" and "Deny" | The host, with "Allow and try again" |
| After a yes | The host goes on the list and the waiting command carries on | The host goes on the list, and a next turn tells Codex that it may try again |
| After a no, or no answer | The command is told the host was refused. The turn waits for an answer until "Stop" | Nothing more happens |

A chat that waits for its owner is marked in the sidebar.

## How it works

1. **The list is kept on the chat,** on the server, and sent to the machine with each turn.
2. **`agent-run` starts each program with it:** Claude Code with the hosts in its sandbox settings, and able to ask through its input and output; Codex with its sandbox proxy on and each host set to `allow`.
3. **A wanted host becomes an event,** the same for both agents: the host, and for Claude Code an id to answer under. Claude Code's comes from its request; Codex's is read from the refusal its proxy prints into the command's output.
4. **The owner's answer goes to the server,** which puts the host on the list. For a waiting Claude Code turn it sends the answer to the machine, and the core hands it to the program. For Codex it starts the chat's next turn with a fixed sentence saying the host is allowed now.
5. **An event says what was answered,** so the chat reads the same later and for teammates.

A machine that is away can't be answered: the button says so. A core restarted in the middle of a turn loses the turn as today, and the question with it; the host can still be allowed afterwards.

## What an allowed host can be used for

A command can send to an allowed host whatever it can read, and both sandboxes let a command read any file its person can, which plan 21 wrote down and left. With no network that was harmless. With one, an agent misled by a repository's text could send a file from outside the chat's folder to a host the person allowed. So:

- **Hosts are exact names,** never a pattern such as `*.com`.
- **The button says what a yes means:** that commands in this chat can then send data to that host.
- **Keeping commands from reading outside the folder** is the plan that should follow this one. It is not in this one because the tools an agent runs need parts of the person's home, and which parts takes its own finding out.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/agent-run/` | Both programs started with a list of hosts; Claude Code's request and Codex's refusal become one event; an answer goes back in to Claude Code. Its tests' stand-in learns to ask and to wait |
| `crates/core-link/` | The hosts with a turn; the event for a wanted host and the one for its answer; a message that carries an answer to the core |
| `apps/core/` | Hands an answer to the turn it belongs to |
| `apps/server/` | `chats.rs`: allowing, denying and removing a host, for the chat's owner. One migration: a chat's hosts, and what it is waiting for |
| `apps/web/` | The wanted host with its buttons, the answer once given, the chat's list of hosts with a way to take one off, and the mark in the sidebar |

## Not in this plan

Keeping commands from reading outside the chat's folder; hosts allowed for a whole project or team; patterns of hosts; letting a command write outside the folder or leave the sandbox; a notification outside the web app that a chat is waiting; a time limit on the wait; "plan" and "build" modes.

## Tests

- **`crates/agent-run/tests/`:** each program is started with the hosts it was given; Claude Code's stand-in asks, and a yes and a no each go back in its own words; Codex's stand-in prints the proxy's refusal, which comes out as the same event; "Stop" while a question waits ends the turn.
- **`apps/server/tests/api.rs`,** with the made-up core: a wanted host shows in the chat and marks it as waiting; the owner's yes puts it on the list, reaches the machine for Claude Code, and starts a next turn with the list for Codex; a no reaches the machine and leaves the list alone; a teammate can do neither; a host that isn't an exact name is refused; a host is taken off the list; a machine that is away says so.

## Steps

- [x] Try for real that each program takes a list of hosts when started as Weacon starts it, and that Claude Code then asks only about others
- [x] `crates/agent-run`: the list, the event, Claude Code's answer
- [x] `core-link` and the core: hosts with a turn, the answer
- [x] The server: a chat's hosts, the answer's route, the next turn for Codex
- [x] The web app: the wanted host, its buttons, the list, the mark
- [x] Docs: `docs/architecture.md` and `README.md`
- [x] Checked by hand with both agents on ContentWrite: a package installed after a yes, and a no

## Outcome

Built as planned, on the recommended answers, with these differences:

- **Codex's proxy is always on,** also for a chat with no hosts, since it is the proxy's refusal that names the host a command wanted.
- **Whatever else Claude Code would ask a person is refused by `agent-run` itself.** Started so that it can ask about hosts, it would otherwise wait for an answer about other things too, which nobody could give.
- **"Allow and try again" shows once the turn has ended,** for either agent: also for a host Claude Code asked about and was refused, or never answered.
- **Tests:** 54 in the server, one new; 6 in `agent-run`, two new.
- **Checked by hand** on the dev server with a real core on ContentWrite. Claude Code asked about `example.com` and waited, the chat showed the buttons and the sidebar its mark, and after "Allow in this chat" the command went on; three more hosts asked for in one turn were each answered. Codex was refused `example.com`, the chat showed "Allow and try again", and its next turn got `200`. `git ls-remote` from GitHub worked in Claude Code's sandbox.
- **Not checked by hand:** "Deny" and taking a host off in the browser, which the tests cover.

## Changed by plan 24, on 24 November

Claude Code no longer asks while its command waits. Started with nobody to ask, it refuses a host and names it as Codex does, so both agents now work the one way described above for Codex, and the answer's message, the waiting chat, "Deny" and the sidebar's mark are gone.

## A cache for each chat, added on 24 November

The plan's last step first failed for a reason that isn't the network: with its registry allowed, `npm install` failed in both sandboxes, because npm keeps its cache in `~/.npm` and a command writes only in the chat's folder. The owner chose a folder of the chat's own over folders allowed by name.

- **Each chat has `cache/` beside its copy,** which both sandboxes let commands write in, and which is no part of the repository.
- **The tools that download packages are pointed at it,** each by the setting it reads: npm, Yarn, Bun, pip, uv, Composer and Go. `agent-run` holds the list. A tool that isn't on it still writes in the person's home and fails.
- **Checked by hand** with a new core: in a Claude Code chat and a Codex chat, `npm install left-pad` installed after the registry was allowed, by "Allow in this chat" and by "Allow and try again". `composer install` reached Packagist and stopped on the project's own PHP version, not on the sandbox.
- **Seen once:** a first Codex turn failed with OpenAI saying a model isn't supported for the account; the same turn sent again ran.
- **Still so:** on macOS the system's `curl` fails its certificate check in Claude Code's sandbox, since it can't ask the system which certificates to trust. Git and Node bring their own and work.

## What the first version found, on 23 November

Tried with Claude Code 2.1.329 and Codex 0.159.2, and checked against Codex's official docs.

- **Claude Code asks for one host at a time** and stays in its sandbox, when started with `--input-format stream-json --permission-prompts host --permission-prompt-tool stdio`. A write outside the folder is refused without asking.
- **`codex exec` asks nobody.** Its `--approve-for-me` hands approvals to an automatic reviewer, not a person.
- **`codex app-server` can ask,** but only to run a command again with no sandbox at all. With Codex's proxy on it still didn't ask about a host; the protocol has such a request, and no page says what turns it on.
- **Nothing leaves the person's settings out under `app-server`.** Only `codex exec` has `--ignore-user-config`, and a profile is laid over the person's `config.toml`, not put in its place. On this machine that let in four MCP servers whose tools run outside the sandbox, a `notify` program, plugins and hooks. Each can be switched off by its name, and nothing switches them all off.
- **Codex's sandbox has a proxy,** `features.network_proxy` with hosts set to `allow` or `deny`. With plain `codex exec` and `--ignore-user-config`, the allowed host answered, another was refused with "domain is not on the allowlist", and a write outside the folder was still refused. This plan is built on that.
- **A conversation started under `exec` resumes under `app-server`,** should Codex one day be moved there.
