# Plan 26: a chat's agent follows the repository's instructions

**Status: done** (2026-11-24)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: in a chat, Claude Code and Codex both follow the instructions a repository keeps for coding agents, and the same ones. Today Codex reads the repository's `AGENTS.md` and Claude Code reads nothing, since it is started so that none of the person's own setup loads, which leaves out the repository's `CLAUDE.md` too. The owner asked on 24 November for this before plan 25.

## The rule

An agent follows the file its own program names, and the other program's where its own is missing:

| The repository has | Claude Code follows | Codex follows |
| --- | --- | --- |
| `CLAUDE.md` and `AGENTS.md` | `CLAUDE.md` | `AGENTS.md` |
| Only `CLAUDE.md` | `CLAUDE.md` | `CLAUDE.md` |
| Only `AGENTS.md` | `AGENTS.md` | `AGENTS.md` |
| Neither | Nothing | Nothing |

A file that names another, as a `CLAUDE.md` that is one line pointing at `AGENTS.md`, is followed through.

## How, tried for real on 24 November

**Claude Code is told in one sentence,** added to its own instructions when it is started: that the repository's instructions are in `CLAUDE.md`, or in `AGENTS.md` where there is none, and to read that file and the files it names before anything else. Tried in a repository with both files and a hook in its settings: it followed `CLAUDE.md` and the `AGENTS.md` that file named, and the hook did not run. In one with only `AGENTS.md` it followed that.

Two other ways were tried and don't work. Started without the flag that leaves the person's setup out, but with no settings files, it read no instructions either. And letting it load the repository's own settings is no option: a repository could then run a program on the machine through a hook, or widen its own sandbox.

**Codex has a setting for it,** which names the files to fall back on. Tried in a repository with only `CLAUDE.md`: without the setting it read nothing, with it it followed the file.

## What stays out

Only the instructions are followed. The rest of what a repository can carry for an agent is not loaded, as today: its settings, hooks, skills, commands and tool servers. The instructions are text the agent could already read in its folder, and what it does with them stays in the sandbox.

The person's own instructions, in their home folder, stay out too.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/agent-run/src/claude.rs` | The sentence, added to how it is started |
| `crates/agent-run/src/codex.rs` | The setting |
| `crates/agent-run/tests/` | Each program is started with it |
| `docs/architecture.md` | What of a repository an agent follows, and what it doesn't |

## Tests

In `crates/agent-run/tests/`: each stand-in is started with what makes it follow the repository's instructions. That the real programs then do can only be checked by hand.

## Steps

- [x] `agent-run`: the sentence for Claude Code and the setting for Codex
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand with both agents, on a repository with each of the two files alone and with both

## Outcome

Built as planned.

- **Tests:** 5 in `agent-run`, two of them grown: each program is started with what makes it follow the repository's instructions.
- **Checked by hand, with the real programs started with the same flags outside Weacon,** in made-up repositories: Claude Code with both files followed `CLAUDE.md` and the `AGENTS.md` it named, and a hook in the repository's settings did not run; Claude Code with only `AGENTS.md` followed it; Codex with only `CLAUDE.md` followed it, and read nothing without the setting.
- **Checked through Weacon** on ContentWrite, which has neither file: both agents said so and went on without.
- **Not checked:** a repository with instructions through Weacon itself, since the dev team's only project has none; Codex with only `AGENTS.md` or with both, which is what it did before this plan.
