# Weacon: notes for coding agents

Weacon is a desktop harness for coding agents with tasks, docs and team chats built in. A core on your Mac, PC or a VPS drives each person's own Claude Code, Codex and other agents; a desktop app, a Chrome extension and later a phone app are its clients; team workspaces sync through a team server. Everything is Rust except the Chrome extension, which is TypeScript.

The default branch is `main`: start from it and open draft PRs against it. This file is for every coding agent: Claude Code, Codex and Amp read it, and `CLAUDE.md` imports it.

## Rules that must always hold

1. **Agent sign-ins never touch Weacon.** Never read, store, log, copy or forward Claude Code, Codex, `gh` or any other vendor credential. Weacon only reads sign-in status. Agents run as the unmodified vendor binaries, and people sign in through the vendor's own tool, against their workspace's config folder.
2. **Workspaces never mix.** An agent runs only with a sign-in from its chat's workspace. Every stored row carries its workspace id; on the team server, Postgres row-level security enforces it and the app role can't bypass it.
3. **Nothing listens publicly but the team server.** The core's socket is reachable only by its user. Local HTTP, such as MCP, binds to 127.0.0.1 and checks a per-run token.
4. **No content in logs or error reports.** Messages, prompts, agent output, file contents, diffs and credentials never reach a log line or an error report.
5. **Every build is identifiable.** Every program reports its version and the commit it was built from, and logs and reports errors from its first commit.
6. **Schemas first.** Every message that crosses a process boundary is a serde type in `crates/protocol` before it is used, and is parsed where it arrives. Schemas only grow: new fields are optional and nothing is removed. Enums carry a tag; no untagged unions.
7. **Proposals, not writes.** Changes that agents or Weacon's AI make to docs and tasks arrive as proposals a person accepts.
8. **Draft first, then review.** Every change reaches `main` through a PR, opened as a draft, unless the owner explicitly asks for something else. Once steps 1 to 8 of the definition of done pass, the agent marks it ready for review, which starts CodeRabbit's review, and fixes what it finds. Agents never merge.
9. **One of each.** Never add a second way to do something the code already does; change the existing way, in its own PR.

## Code boundaries

`tooling/archtest` enforces these from `tooling/archtest/src/rules.rs`, which lists the crates that exist today. A new crate needs a row there, tests, and a manifest that takes its lints, package fields and dependency versions from the workspace.

| Crate or app | May use | Does I/O |
| --- | --- | --- |
| `crates/protocol` | nothing in the repo | no |
| `crates/observability` | `protocol` | no |
| `crates/version` | nothing in the repo | at build time: it runs `git` |
| `crates/model` | `protocol` | no |
| `crates/agents` | `protocol`, `model`, `observability` | yes |
| `crates/client` | `protocol`, `model`, `observability` | yes |
| `crates/testing` | anything; only as a dev-dependency | yes |
| `apps/core` | `protocol`, `model`, `client`, `agents`, `observability`, `version` | yes |
| `apps/server` | `protocol`, `model`, `observability`, `version`; never `agents`, because the team server never runs agents | yes |
| `apps/desktop` | `protocol`, `model`, `client`, `observability`, `version` | yes |
| `tooling/*` | nothing in the repo | yes |
| `apps/extension` | TypeScript; only the types generated from `protocol` | — |

- Apps never depend on other apps. The desktop talks to the core over its socket, never by linking it.
- Every app depends on `observability`, and on `version` both as a dependency and as a build-dependency whose `emit()` its `build.rs` calls.
- A crate without I/O never touches `std::fs`, `std::net`, `std::process`, `std::env` or an async runtime; it takes what it needs as arguments.
- Only the `db` module (`src/db.rs` or `src/db/`) of `apps/core` and `apps/server` uses a database driver, and only those apps may depend on one.
- Only a `platform` module (`src/platform.rs` or `src/platform/`) selects an operating system with `cfg`; in the core that is `apps/core/src/platform`.
- Only `crates/agents` starts agent processes.
- Only `crates/observability` depends on Sentry's crates.
- A source file holds at most 400 lines before its test module, and no TODO or FIXME: finish the work, or leave the note out.

## Code style

- **Original code only:** never copied, renamed or translated from another project.
- **Simple, clear, terse.** Write the shortest code that reads in one pass, never the cleverest: `?` and early returns, `let ... else`, `if let ... &&` chains, iterator chains that read top to bottom, `matches!`, field shorthand. Small functions (clippy stops at 50 lines, 5 arguments and 4 levels of nesting), plain data, precise types. Owned data and plain structs before trait objects, generics with one implementation, or `Arc<Mutex<_>>`. Nothing built for a need nobody has. `pub(crate)` unless another crate needs it. Modules are `name.rs` beside a `name/` folder, never `mod.rs`.
- **No panics outside tests.** No `unwrap`, `expect`, `panic!` or `unreachable!`; no indexing or string slicing that can panic, so use `get`; no `as` casts, so use `From` and `TryFrom`. Return an error instead. No `unsafe`.
- **Suppressions** are `#[expect(lint, reason = "...")]`, never `#[allow]`, so a stale one fails the build.
- **Edges and insides.** Parse data with `protocol` types where it enters a process; inside, trust the types.
- **Side effects as arguments.** Crates take clocks, id generators, file systems and process spawning as arguments; only apps wire in the real ones.
- **Async.** tokio, in the apps and the crates that do I/O. Never block inside async code (use `spawn_blocking`), never hold a lock across `.await`, and keep every `select!` branch safe to cancel.
- **Comments are few, short and to the point.** Write one only where a reader would otherwise get it wrong: a constraint not visible locally, a workaround for someone else's behaviour, a line that looks like a mistake and isn't. Never narrate the next line, restate a name, list what the code or its tests already show, tell history, re-argue a decision or cite a document. A comment is one line, two at most; a doc comment states the contract only where the signature doesn't, plus the sections and examples clippy and doctests need. Prefer a better name to a comment.
- **Async results:** a stale result never overwrites a newer one; give each request a number and drop answers to old ones.
- **Refreshes** never clear what they refresh: fetch, then replace; show loading only when nothing is there yet; keep good data when a fetch fails.
- **Command lines:** ids and paths are checked before they reach a command line, so none can be read as an option.
- **Hooks and helpers** that agents run never block them: when Weacon isn't there, they print nothing and exit cleanly.
- **Screenshots and demos** use made-up data, never real project names.
- **Errors** that Weacon raises are a `Fault` from `crates/observability`: a dotted code from `crates/protocol`, written `code!("store.full")` so it is checked when compiled, and a short fixed message with no user content in it; these are the only messages that reach logs. A crate may have its own error enum and convert it into a `Fault` where it is logged or crosses a process boundary.
- **Tests** sit at the end of the file they test, in a `#[cfg(test)] mod tests`, and are deterministic: no sleeps, no fixed ports, no shared state between tests, temporary folders, and per-worktree state in `.dev/`. They test behaviour strongly enough that the mutation tests find no surviving mutant.
- **UI copy:** short, plain sentences in sentence case. Vendor names in plain text, no vendor logos.

## Logs, error reports and versions

Every program logs, reports errors and says which build it is from its first commit, through `crates/observability` and `crates/version`.

- **One logger.** Create a `Logger` once in each app and pass it down; take a `child` for a component or a request. No `println!`, `eprintln!` or `dbg!` anywhere: clippy rejects them.
- **Start with the build.** Bind `name`, `version` and `commit` from `weacon_version::version!()` on the root logger, and log `app started` first.
- **Events, not sentences.** The message is a fixed `&'static str` naming what happened (`run started`, `sync failed`). Values go in fields: `log.info("run started", &[("run_id", run_id.into()), ("agent", agent.into())])`.
- **Ids for context.** Bind `workspace_id`, `chat_id`, `run_id` or `request_id` where they apply, so one search finds a whole flow.
- **No content, ever.** The logger replaces known secret and content fields, but that is a safety net, not permission.
- **Errors once.** Log an error where it is handled, as `("err", Field::error(&error))`, never again on the way up. Only a `Fault` keeps its message; other errors are described by type, code, I/O error kind and causes.
- **Libraries' logs** through `log` or `tracing` reach the same sink with their level and target only; their messages are dropped.
- **Levels.** `fatal`: about to exit. `error`: a person needs to look. `warn`: failed and handled. `info`: lifecycle, the default. `debug`: developer detail. `trace`: very noisy.
- **Where logs go.** The core: rotated JSON lines in its state folder, and stderr in a terminal. CLI commands: warnings and errors on stderr, `--verbose` adds info. The team server: stdout, one record per request (method, route template, status, duration, request id). The desktop: rotated JSON lines in its logs folder. The extension: forwarded to the core.
- **Error reports** use the Sentry protocol, off without a DSN. On people's devices the desktop and the core send them only after the person agrees, and Settings can turn them off. `crates/observability` builds and scrubs each report (`Reports::upload`) and the app sends it, so only apps touch the network; report errors with `capture_error` and a `code` tag, never messages. A DSN in `WEACON_SENTRY_DSN` turns them on. The release is `version!().release()`.
- **Versions** come only from `weacon_version::version!()`: `weacon --version`, `GET /version` on the team server (`{"name", "version", "commit"}`, `Cache-Control: no-store`), Settings › About in the desktop, and the extension's `version_name`.
- **Tests.** Each app has a test that puts marker strings in a message, a token and a file path, triggers a log line and an error report, and checks that no marker reaches either.

## Setup and commands

`scripts/setup.sh` installs everything a fresh machine, cloud session, orb or CI runner needs, and is safe to run again:
- Rust at the version in `rust-toolchain.toml`, through rustup. Where rustup can't download it, as in Claude Code cloud sessions, it uses the installed stable toolchain if that is exactly the pinned version.
- The dependencies, and `llvm-tools` for coverage where it can be downloaded.
- The check tools at pinned versions: cargo-deny, cargo-machete, cargo-llvm-cov and cargo-mutants, built from crates.io the first time.
- Its output goes to `.dev/setup.log`.

| Where the agent runs | How it is set up |
| --- | --- |
| Claude Code cloud session | The `SessionStart` hook in `.claude/settings.json` runs `scripts/setup.sh --claude-session-start` (toolchain and dependencies); run `scripts/setup.sh` once before the first `scripts/check.sh` |
| Codex cloud environment | The environment's setup script is `scripts/setup.sh` |
| Amp orb | `.agents/setup`, a link to `scripts/setup.sh`, runs when the orb's snapshot is built |
| A laptop, local agents and CI | `scripts/setup.sh` |

| Command | What it does |
| --- | --- |
| `scripts/setup.sh` | Installs or updates everything; safe to run again |
| `scripts/fix.sh` | Formats and applies the lint fixes clippy is sure of |
| `scripts/check.sh` | What CI runs: rustfmt, clippy with warnings as errors, docs, cargo-machete, cargo-deny, all tests with the 90% line-coverage floor, doc tests, mutation tests on the code changed against `origin/main`, and ShellCheck |
| `cargo test -p <crate> [name]` | Focused tests while iterating |
| `WEACON_STATE_DIR=.dev/state cargo run -p weacon-core -- doctor` | Runs the core with this worktree's own state folder |
| `UPDATE_SCHEMA=1 cargo test -p weacon-protocol` | Regenerates `crates/protocol/schema.json` after a message changes |

Every dependency is declared once, in `[workspace.dependencies]` in the root `Cargo.toml`; crates refer to it with `workspace = true`. `Cargo.lock` is committed and every command runs with `--locked`. A new dependency must be at least three days old, maintained, under a licence `.cargo/deny.toml` allows, and without a build script unless it needs one; say why it's needed in the PR.

## Definition of done

Every change goes through these steps, in order, before it's committed. The `finish` skill walks through them. Skills live in `.agents/skills/`, which Codex and Amp read; `.claude/skills/` links to them for Claude Code.

1. **Code and tests.** Every behaviour change has a test that fails without it; every bug fix has a regression test; every new crate has tests.
2. **Simplification review.** An independent pass at three levels: the file, the feature, the project (`.agents/skills/simplify-review`). Fix every *must fix*; fix the rest or say why not.
3. **Tests.** Run the tests of every crate the change touches, and make them pass.
4. **Correctness and security review.** An independent pass on the simplified code (`.agents/skills/correctness-review`). Fix every *must fix* with a test; fix the rest or say why not.
5. **Local CI.** `scripts/fix.sh`, then `scripts/check.sh`, which must pass in full. Never skip a step, and never add a suppression without a reason.
6. **Rules.** If a rule, a boundary or a command changed, this file says so in the same change.
7. **Commit.** A short subject in the imperative, under 72 characters. Add a body only when the change needs one: a few lines on what changed and why. Commit messages never link to documents or other projects.
8. **Draft PR.** Push the branch and open or update a draft PR against `main`.
9. **Review.** Mark the PR ready for review. Once CodeRabbit has reviewed the latest commit, check each finding against the current code, fix the valid ones with a test and steps 3 to 7, answer the rest with a one-line reason, resolve each addressed thread, and push. Security findings you reject stay open for the owner. Repeat after each new review until no other findings are open, then leave the PR to the owner.

Each review runs as an agent that didn't write the change and hasn't seen its author's reasoning: in Claude Code the `simplifier` and `auditor` subagents, in Codex `codex exec --sandbox read-only "Follow .agents/skills/simplify-review/SKILL.md on the current change"`, in Amp a subagent given the same instruction.

## Working in parallel

Several agents work on this repository at once: Claude Code, Codex and Amp, on laptops and in their clouds, each in its own worktree, session or orb.

- **One task, one branch, one worktree.** Name branches `<agent>/<topic>` (`claude/core-skeleton`, `codex/protocol-framing`, `amp/team-server`). Start from a fresh `origin/main` and rebase on it before committing.
- **Open a draft PR early,** as soon as the first commit exists.
- **Stay in your lane.** A task owns the folders it names. Touching others, this file, root configs or `tooling/archtest/src/rules.rs` goes in its own small PR.
- **Small PRs.** Each should be reviewable in a few minutes. The owner squash-merges.
- **No shared append-only files.** No changelog. Generated files are regenerated after a conflict, never merged by hand; for `Cargo.lock`, take `main`'s and run cargo again.
- **Nothing fixed per machine.** Random ports, temporary folders, caches under the user's cache directories, and per-worktree state in `.dev/`; each worktree builds into its own `target/`.
