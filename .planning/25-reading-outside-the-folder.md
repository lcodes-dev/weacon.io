# Plan 25: nothing read outside a chat's folder

**Status: planned**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: an agent in a chat, and the commands it runs, read the chat's own folders and what their tools need, and nothing else of the person's. Since plan 23 a command can reach the hosts its chat allows, and both sandboxes let it read any file the person can, so an agent misled by a repository's text could send a key, a token or another project to an allowed host.

## What a chat may read, after this plan

| | Read |
| --- | --- |
| The chat's copy of the repository and its cache | Yes, as now |
| The system, and the programs and libraries installed on it | Yes |
| The tools the person installed in their home, such as Node from a version manager | Yes |
| The settings a tool reads to work at all, such as `~/.gitconfig` | Yes |
| Anything else in the person's home: keys, tokens, shell history, documents, other programs' settings | No |
| Other disks and other projects | No |

Writing doesn't change: the copy and the cache, as now.

## What each program offers, tried for real on 24 November

**Claude Code has one switch for it,** `permissions.blockReadsOutsideWorkingDirectories`. With it on, tried as Weacon starts the program:

- A command could not list the home folder, `~/.ssh`, `~/Documents`, `~/.config` or the external disk, and could not read `~/.npmrc`, `~/.aws/credentials`, the GitHub CLI's token file, the shell's history or Codex's sign-in.
- `~/.gitconfig` stayed readable, which git needs.
- Node, npm, git, Python and PHP ran, the first two from a version manager in the home folder. The chat's cache stayed readable.
- Claude Code's own file tool was refused a file outside the folder, which the sandbox alone doesn't do.

Which paths stay open is Claude Code's own list, kept by its maker.

**Codex has no such switch, and needs a list of Weacon's making.** It has permission profiles: a table of paths, each `read`, `write` or `deny`, where the more specific path wins. Tried, passed on the command line beside `--ignore-user-config`:

- A profile of only what Codex calls the minimum stopped Codex itself from starting.
- Codex's usual sandbox with the home folder and other disks denied, and let back in the chat's folders and the folder beside each program on the `PATH`: the copy and the cache worked, Node, Python and PHP ran, the home folder and the other disk were refused, an allowed host answered and another was refused by name as today. But `git` and `npm` failed, most likely on their settings in the home folder, which this plan's first step has to settle.
- A profile can't be combined with the flags Weacon uses today for Codex's sandbox, so those move into the profile: the folders it may write and the hosts it may reach.

## What is proposed

- **Claude Code:** the switch, and nothing else.
- **Codex:** a profile that `agent-run` builds for each turn: Codex's usual sandbox; the person's home and other disks denied; the chat's copy and cache writable; readable again, the folder beside each program on the `PATH` that lies in the home folder, and the few settings files tools need, as far as the first step finds them.
- **The difference is said in the docs:** what stays readable is Claude Code's list on one side and Weacon's on the other, and they won't be the same.

If the first step finds no list under which git and the package tools work in Codex, the plan stops there and comes back to the owner, as plan 23 did.

## What it costs the person

- **An agent can't look at another project** on the machine for reference, or at a file the person points it to outside the chat.
- **A tool that keeps what it needs somewhere unusual in the home folder fails in Codex** until its folder is on the list. In Claude Code that is its maker's concern.
- **Credentials the person relies on are gone for commands:** a private registry's token in `~/.npmrc`, SSH keys for a private dependency. That is the point of the plan, and it will break some installs that work today.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/agent-run/src/claude.rs` | The switch, in the settings it already passes |
| `crates/agent-run/src/codex.rs` | The profile in place of today's sandbox flags, built from the chat's folders, its hosts and the `PATH` |
| `crates/agent-run/tests/` | Each program is started with its limit; the profile's paths for a made-up `PATH` |
| `docs/architecture.md` | What a chat may read, and that the two agents' lists differ |

Nothing changes in the core, the server or the web app.

## Not in this plan

Letting the owner allow a folder to be read, as with hosts; credentials for private registries and private repositories in a chat; Linux and WSL, where both sandboxes work differently and neither was tried; anything about writing.

## Tests

In `crates/agent-run/tests/`, with the stand-in: Claude Code is started with the switch; Codex is started with a profile that denies the home folder, lets the chat's copy and cache be written, lets the folders beside the home's programs on the `PATH` be read, and carries the chat's hosts. What the real sandboxes then refuse can only be checked by hand.

## Steps

- [ ] Find the list under which git and the package tools work in Codex with the home folder denied, and stop if there is none
- [ ] `agent-run`: Claude Code's switch
- [ ] `agent-run`: Codex's profile in place of its sandbox flags
- [ ] Docs: `docs/architecture.md`
- [ ] Checked by hand with both agents on ContentWrite: a key, a token and another project can't be read; a package still installs; a refused host is still named
