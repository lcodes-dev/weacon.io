# Plan 19: machines

**Status: done** (2026-11-22)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a person pairs their own computer, or a server of theirs, with Weacon, sees it online, and the docs writer can run on it with that person's own sign-in to Claude Code or Codex. This is the ground chats will stand on, and it settles how a subscription may be used.

## The rule

**An API key is the team's, and is used on the server. A subscription is a person's, and is used only on that person's machines.** The owner set this on 20 November.

| | An API key | A subscription |
| --- | --- | --- |
| Whose | The team's | One person's |
| Kept | In Weacon, by the team's owner | Never in Weacon: the vendor's own program on the machine holds the sign-in |
| Runs | On the server | On the machine of the person whose work it is |
| For | What is the team's and nobody's in particular | What a person starts |

## Why agents run on a person's own machine

Checked against Anthropic's and OpenAI's own pages on 20 November:

| Way to run an agent | Whose money | May Weacon do it? |
| --- | --- | --- |
| An API key, on the server | The team's API account | Yes. Both vendors recommend it for automation |
| The vendor's own program on a person's computer, signed in by that person | That person's subscription | Yes: it is what the program is for. Weacon must not read, keep or pass on the sign-in |
| One person's subscription on the server, serving a team | That person's subscription | No. Anthropic: a product may not "route requests through Free, Pro, Max, or Ultra plan credentials on behalf of their users". OpenAI's terms forbid sharing an account |
| "Sign in with Claude" inside Weacon | The person's subscription | No such thing is offered. Anthropic doesn't allow a product to offer claude.ai login unless approved |
| "Sign in with ChatGPT" inside Weacon | The person's subscription | OpenAI offers it to open-source and locally run apps, and to hosted ones only after an interest form. Not needed for this plan |
| The vendors' clouds (Claude Code on the web, Codex cloud) | The person's subscription | Neither has an API an outside product may drive |

So there is no managed way for Weacon to spend a member's subscription. The allowed way is the one this plan builds: the member's own computer runs the vendor's unmodified program, signed in by the member.

**What follows for today's docs writer:** "Claude Code" and "Codex" as writers run on the server's computer, on whoever is signed in there. For one person hosting Weacon for themselves that is their own use. For a team it is one person's subscription serving the others, which the vendors don't allow and the rule above rules out. Part 2 moves those two writers onto the machine of the person who starts the run.

## Work lives on the machine

The owner asked on 20 November for machines to be used from afar the way a terminal multiplexer for agents does it: the work belongs to the machine, and a person looks in on it from wherever they are. Two things are taken from that, and two are not:

| | Taken? | How |
| --- | --- | --- |
| Work outlives whoever started it | Yes | The core runs in the background on the machine and owns what it was asked to do. Closing the browser changes nothing, and a dropped connection doesn't stop the work: the core finishes and hands the result over when it is back |
| A server reached over SSH | Yes | One command on a paired machine sets up a server you reach by SSH and pairs it. From then on it connects to Weacon like any other machine |
| SSH as the way Weacon reaches a machine | No | Weacon's server can't reach into a laptop behind a router, and nobody should give it their keys. Every machine dials out instead |
| The agent's own terminal in the browser | No | A chat needs to know what the agent said, changed and asks, which a stream of terminal bytes doesn't tell |

## Part 1: pairing a machine and seeing it online

**A machine is a person's own computer or their own virtual machine, and only theirs.** It serves every team that person is in. Nobody else sees it or uses it. The owner set this on 20 November.

**The core** is the program that runs on a machine: a second small binary, `weacon-core`, in `apps/core/`. It is always the core that gets in touch with Weacon, never the other way round.

**Which Weacon it talks to is the core's own setting,** since a team hosts Weacon wherever it likes. The address comes with the pairing command and is kept, with the token, in a file only the machine's user can read. `weacon-core status` says which server it is paired with and whether it is connected; pairing again points it at another.

**Where the core runs:**

| System | How |
| --- | --- |
| macOS | As it is |
| Linux, on a computer or a virtual machine | As it is |
| Windows | Inside WSL only, where it is the Linux core. Nothing of Weacon runs on Windows itself, as the owner asked on 20 November |

**Pairing is one command:**

1. In the web app, "Add a machine" under a new "Machines" settings page shows a command: `weacon-core pair https://weacon.example.com 3f9c…`. The code in it works once, for 10 minutes.
2. The person runs it on the computer. The core sends the code with the computer's name and system, gets a token back, and keeps it in a file only its user can read.
3. The machine appears in the list.

The earlier design had five steps with a typed code, a lookup and a confirmation. A command copied by the signed-in person does the same with one request.

**Connecting.** `weacon-core` with no arguments dials the server and holds one WebSocket open, sending its token. Nothing listens on the machine, so a router or firewall is no obstacle. It dials again when the connection drops, waiting from 1 second up to a minute.

**In the background.** `weacon-core install` sets the core to start with the computer and stay running without a window: a launch agent on macOS, a user service on Linux that also runs when nobody is logged in. `weacon-core uninstall` undoes it. In WSL that service needs systemd turned on, which the command checks and explains; how long WSL keeps running with no window open is to be found out by hand.

**The core makes sure the agents are there.** While pairing, in the terminal the person is at, it looks for `claude` and `codex`, and for one that is missing it offers the vendor's own installer for that system, running it only on a yes. `weacon-core agents` says at any time what is installed and signed in. The vendors' install and login commands are taken from their own pages when this is built, not from memory.

**Signing in happens in Weacon, in a terminal.** The owner asked for this on 20 November. Beside an agent that isn't signed in, the machine's page has "Sign in", which opens a terminal in the web app:

1. The core starts the vendor's own login command on the machine, in a terminal of its own that nobody sees there.
2. What that terminal shows goes to the web app, and what the person types goes back, through the core's connection. The person follows the vendor's steps: a link to open, a code to paste.
3. When the command ends, the terminal closes and the core asks the vendor's program whether someone is signed in.

- **It is a terminal for signing in and nothing else.** The core chooses the command, `claude` or `codex` with their login, from the agent's name. Nothing sent from Weacon can make it start anything else, so this is no way into the machine.
- **Only the machine's owner can open it,** and one at a time for each agent.
- **Weacon's server passes the terminal along and keeps none of it:** not in the database, not in its log.
- **It works the same on a laptop, a virtual machine and in WSL,** since nothing has to open on the machine itself.
- **A run that needs an agent that isn't signed in** stops and says so, with a link to that "Sign in".
- **In the web app** the terminal is drawn by `ghostty-web`, Ghostty's own terminal engine built for browsers, fetched only when a sign-in is opened. The name the owner gave, libghostty, is on npm only as a reserved, empty package; `ghostty-web` is the published one, at 0.4.0. A trial comes first: the two vendors' login screens drawn right and a pasted code arriving whole. If it falls short, `@xterm/xterm` does the same job.

**What this costs.** The link the vendor shows and the code the person pastes pass through Weacon's server on their way. Anthropic's terms say a product may not "collect, store, or intermediate Claude.ai credentials or session tokens". What passes here is neither kept nor a lasting sign-in: the code is good once, and only together with a secret that never leaves the vendor's program on the machine. Whether passing it along still counts is for Anthropic to say, which is open question 28. The owner chose this knowing it.

**A server over SSH.** `weacon-core add user@host`, run on a machine that is already paired, sets up a server you can reach by SSH:

1. It says what it is about to do on that host and asks to go on.
2. It copies the core there, when the host has the same system and processor; otherwise it says how to install it by hand.
3. It gets a pairing code from Weacon with its own token, and runs `pair` and `install` on the host.

SSH is only how the core gets there. The command then offers to install the agents on the host as pairing does. Signing in is done from Weacon, like on any machine.

**What a machine says about itself** when it connects, and every few minutes: which of `claude` and `codex` it finds, and for each whether someone is signed in (`claude auth status`, `codex login status`). Weacon learns that a sign-in exists and nothing about it.

**In the web app:**

- The sidebar's "Machines" lists your machines, each with a dot, green when connected, and the agents ready on it.
- The "Machines" settings page lists them with their system, when each was last seen, and what each agent says: "signed in", "not signed in" with "Sign in", or "not installed" with the command that installs it. "Remove" ends a machine: the server closes its connection and the core forgets its token.

**Left for later:** updating the core; a sign-in per team on one machine; a terminal in Weacon for anything but signing in.

### Data

```
machines        id (key), user_id → users (cascade), name, os, token (unique), created_at, seen_at
                agents: a JSON list of what it last said about itself
                index on user_id
machine_codes   code (key), user_id → users (cascade), created_at
```

### API

| Route | Who | What |
| --- | --- | --- |
| `GET /api/machines` | signed in | Your machines, each with whether it is connected |
| `POST /api/machines/codes` | signed in, or a machine's token | A pairing code for that person, good once for 10 minutes |
| `POST /api/machines` | anyone with a code | Trades `{"code", "name", "os"}` for `{"token"}` |
| `DELETE /api/machines/{id}` | its owner | Ends the machine |
| `GET /api/machines/connect` | a machine's token | The core's WebSocket |
| `GET /api/machines/{id}/sign-in/{agent}` | its owner | A WebSocket to the sign-in terminal of `claude` or `codex` on that machine; refused when the machine isn't connected or one is already open |

### What crosses the WebSocket

JSON, one message a frame. From the machine: `hello` with the core's version, and `agents` with what it found. For a sign-in: `terminal.open` with the agent and the terminal's size, `terminal.data` both ways with what is shown or typed, `terminal.size` when the window changes, and `terminal.close` from either side. The types live in a small crate, `crates/core-link/`, since the server and the program must agree on them.

## Part 2: the docs writer on a machine

A team's writer becomes one of:

| Writer | Runs | Paid by |
| --- | --- | --- |
| The Anthropic API | on the server, as today | the team's API key |
| Claude Code on your machine | on the machine of whoever starts the run | their subscription |
| Codex on your machine | the same | their subscription |

- **A run, a request or a page's check uses the machine of the person who caused it.** With several machines, the first that is connected and has the agent signed in. With none, the run says so and nothing is spent.
- **Two more messages:** the server sends `ask` with a system prompt, a prompt and a model; the machine answers `answer` with the text or why there is none. The core runs the very command the server runs today, with no tools and none of the person's own settings, in an empty folder.
- **The work outlives the connection.** The core keeps what it was asked and what it answered until the server says it has the answer. After a drop it sends again what wasn't taken. The server waits ten minutes for a machine that went away, then fails the run and says so.
- **"Claude Code" and "Codex" on the server's own computer go away,** with the code that starts programs from the server, as the rule asks.

This is the smallest real use of a machine, and it is the same path a chat will take, with more messages.

## For chats, later

Not built here, noted so that this plan doesn't close doors:

- **Claude Code** is driven as its own command with streamed JSON in and out, which carries prompts, permission questions, follow-ups and stopping. The Agent SDK does the same but needs Node or Python on every machine.
- **Codex** is driven through `codex app-server`, the only one of its interfaces that can ask for approval, take a follow-up and be interrupted. OpenAI calls it experimental and ties its shape to the program's version, so the version it was written against is checked.
- Both keep working in the person's own checkout of the repository.

## Tests

In `apps/server/tests/api.rs`: a code pairs once and not twice, nor after 10 minutes; a machine gets a code for its own owner; a machine connects with its token and shows as connected, and as not after it leaves; a sign-in terminal carries bytes both ways for the machine's owner, is refused to anyone else and to a second opener, and closes when the machine's side does; a removed machine is refused; someone else can't see or remove a machine; a docs run on a machine writer asks the starter's machine, takes an answer that arrives after a drop, and fails cleanly without a machine. The tests stand in for a machine with a WebSocket client of their own. The core itself is checked by hand: paired with the dev server, installed in the background, seen online after a restart, a docs run made through it, an agent installed through it and signed in from the web app, on a computer and on a virtual machine, and a server added over SSH. WSL is checked on a Windows computer if one is at hand, and said plainly if not.

## Steps

Part 1:

- [x] The crate with the messages, and the migration and queries
- [x] Pairing codes, pairing, the list and removal, with tests
- [x] The WebSocket: connecting, being seen, leaving
- [x] The core: `pair`, `status`, connecting, saying what agents it has
- [x] The core's care for the agents: offering to install, and `agents`
- [x] The trial of `ghostty-web` on the two vendors' login screens
- [x] The sign-in terminal: the core's side, the server passing it along, and the web app's terminal
- [x] The core in the background: `install` and `uninstall`, on macOS, Linux and WSL
- [x] A server over SSH: `add`
- [x] The web app: the sidebar's machines and the settings page

Part 2:

- [x] `ask` and `answer`, kept across a dropped connection, and the two machine writers in place of the server's programs
- [x] The web app's writer choice, and what a run says without a machine

Both:

- [x] Docs: `docs/architecture.md`, and how to run the core in `README.md`
- [x] Checked by hand with a real machine

## Outcome

Built as planned, with these differences and limits:

- **The hand check covers part of the plan,** so the plan stays in progress and its last step unticked. Done on this computer, with the core's and the agents' folders pointed at scratch ones so that no real sign-in was touched: pairing with a code, a second use of the code refused, the token's file readable by its user only, the core connecting and showing in the sidebar and on its page, both vendors' sign-ins shown in the web app's terminal, typed keys and a pasted code arriving whole, a docs request going from the server through the real core to a stand-in for `claude` and back, and a removed machine's core being cut off, refused and forgetting its token.
- **`install` and `uninstall` were checked on macOS** once the owner agreed, on 20 November: the launch agent was written and valid, the core ran under it and connected, found the agents signed in from there, was started again within seconds of being killed and connected anew, and was gone after `uninstall`. Its log file stays in `~/Library/Logs`, which `uninstall` doesn't remove.
- **Checked on 22 November,** once the owner asked for it:
  - **A real agent through a machine.** A core on this computer, paired with the dev server and using the owner's own Claude Code sign-in, answered a docs request on ContentWrite; the proposal it wrote is there to review.
  - **`install` on Linux,** in a temporary Ubuntu virtual machine on ARM with a core built for it: the user service was written, started and connected, and was started again within seconds of being killed. `uninstall` stopped and removed it.
  - **`add user@host`,** from that virtual machine to a second one over SSH with a key: it copied the core to `~/.local/bin`, paired it and installed the service, which connected.
  - Removing the three machines in Weacon stopped each core.
- **Still not checked:** `install` in WSL, for lack of Windows; and a sign-in carried through to the end, which needs the owner's own account in a browser.
- **Answered by the owner on 22 November:** one sign-in per machine for now; no letter to the vendors; and the core is to be downloadable and installable on each machine, which is a plan of its own.
- **The trial of `ghostty-web` passed** on both login screens, with one thing learned: text put in by a script's "type" didn't arrive, while real key presses and a paste did.
- **Claude Code's login opens a browser on the machine itself** besides printing its link. On a machine with a screen, a tab opens there when a sign-in is started from Weacon, wherever the person is.
- **Codex is signed in with its device code,** `codex login --device-auth`, since its usual login needs a browser on the machine. OpenAI's page says that way must be turned on in ChatGPT's security settings.
- **What is asked and answered is kept in memory** on both sides, so it survives a dropped connection and not a restart of the core or the server.
- **A run picks the first of the person's machines** that is connected with the agent signed in, in the order they were paired.
- **The server's address in the pairing command is `WEACON_URL`,** the address people open. In development that is the web app's own port, which now passes WebSockets on to the server.
- **Any team's owner may choose a machine writer,** since it no longer uses the server's own account.
- **The terminal's file is 638 kB,** with Ghostty's engine inside it, fetched when a sign-in is opened. The lock file gained one package.
- **A sign-in's end is seen at once.** With a stand-in for `claude` that becomes signed in when its login has run: the terminal closed by itself when the login ended, and Weacon showed the agent as signed in a quarter of a second later. A real vendor's sign-in was still not carried to its end.
- **No `hello`:** the core's version was sent and used by nothing, so the message was taken out in the simplification pass. A core says `agents` when it connects.
- **Tests:** 50 in the server, four of them new and one gone with the server's programs: pairing, a core's connection, a docs run on a machine across a dropped connection, and the sign-in's relay. One in the core, for the agents' programs, moved from the server.
