# Weacon: notes for coding agents

Weacon is a web harness for coding agents with tasks, docs and team chats built in. One server, self-hosted or ours, holds every workspace in SQLite and serves the web app; a core on each person's Mac, PC or VPS dials out to it and drives that person's own Claude Code, Codex and other agents. The Chrome extension talks to the local core, and any agent can use Weacon through the server's MCP endpoint. Everything is Rust except the web app and the Chrome extension, which are TypeScript.

The default branch is `main`: start from it and open draft PRs against it. This file is for every coding agent: Claude Code, Codex and Amp read it, and `CLAUDE.md` imports it.

## Rules that must always hold

1. **Agent sign-ins never touch Weacon.** Never read, store, log, copy or forward Claude Code, Codex, `gh` or any other vendor credential. Weacon only reads sign-in status. Agents run as the unmodified vendor binaries, and people sign in through the vendor's own tool, against their workspace's config folder. Weacon never relays a sign-in code, reads another vendor's tokens or imports credential files. The one exception is the sign-in terminal: only when a person asks, their core runs the vendor's login command, with fixed arguments and no shell, under their OS user and their workspace's config folder, and shows it in the web app sealed end to end. The browser checks the core's key against the fingerprint confirmed at pairing, the server relays bytes it can't read, and nothing parses, stores or logs them. Because the server serves the web app's code, this trusts its operator; anyone who doesn't signs in over SSH instead. The core installs a vendor's CLI only when its owner asks, through the vendor's official installer; Weacon never ships one.
2. **Workspaces never mix.** An agent runs only with a sign-in from its chat's workspace. Every stored row carries its workspace id, references between rows include it in their foreign keys, and every query goes through the `db` module's workspace handle. Only a person's account and what belongs to them alone on the server, their sessions, email sign-in challenges and paired machines, are server-wide, and they hold no workspace content. A membership carries its workspace id and refers to its account by id; the `db` module's account handle reaches only that account's own rows and lists its memberships, and everything in a workspace goes through the workspace handle. Membership is checked on every request and by every open feed, so removing it ends access at once, open feeds included, and nothing one workspace sees reveals another or whether an account exists.
3. **Nothing listens publicly but the server.** The core only dials out: to the server, over TLS it verifies against a trusted or pinned certificate, never skipped, proving itself with the machine key it got at pairing. Its socket is reachable only by its user. Local HTTP, such as the MCP the core serves to its own runs, binds to 127.0.0.1 and checks a per-run token. The server's API, and its MCP endpoint for agents Weacon didn't start, accept only signed-in people, paired machines, and tokens a person granted for one workspace, with no more rights than theirs, that expire and can be revoked.
4. **No content in logs or error reports.** Messages, prompts, agent output, file contents, diffs and credentials never reach a log line or an error report.
5. **Every build is identifiable.** Every program reports its version and the commit it was built from, and logs and reports errors from its first commit.
6. **Schemas first.** Every message that crosses a process boundary is a serde type in `crates/protocol` before it is used, and is parsed where it arrives. Schemas only grow: new fields are optional and nothing is removed. Enums carry a tag; no untagged unions. Peers check each other's capabilities, never a version number; only a breaking change raises the protocol version.
7. **Proposals, not writes.** Changes that agents or Weacon's AI make to docs and tasks arrive as proposals a person accepts.
8. **Draft first, then review.** Every change reaches `main` through a PR, opened as a draft, unless the owner explicitly asks for something else. Once steps 1 to 8 of the definition of done pass, the agent marks it ready for review, which starts CodeRabbit's review, and fixes what it finds. Agents never merge.
9. **One of each.** Never add a second way to do something the code already does; change the existing way, in its own PR.
10. **Agents answer to their owner.** Every agent message names its owner and machine. A machine is its owner's alone unless a workspace shares it; on a shared machine each person runs their own core under their own OS user with their own sign-ins, and a workspace's shared agent runs under its own OS user with the workspace's own credential, never a person's. Only an agent's owner, or for a shared agent anyone in its workspace, can ask it to act; asking never runs anything as someone else. Others' words reach an agent as quoted messages, never as instructions. Destructive operations ask a person first, whatever the permission level.

## Code boundaries

`tooling/archtest` enforces these from `tooling/archtest/src/rules.rs`, which has a row for each crate that exists today or that a work item will create, listed in `PLANNED`. A new crate needs a row there and its name out of `PLANNED`, tests, and a manifest that takes its lints, package fields and dependency versions from the workspace.

| Crate or app | May use | Does I/O |
| --- | --- | --- |
| `crates/protocol` | nothing in the repo | no |
| `crates/observability` | `protocol` | no |
| `crates/version` | nothing in the repo | at build time: it runs `git` |
| `crates/model` | `protocol` | no |
| `crates/codemap` | `protocol` | no |
| `crates/agents` | `protocol`, `model`, `observability` | yes |
| `crates/client` | `protocol`, `model`, `observability` | yes |
| `crates/appkit` | `protocol`, `observability`, `version`; only apps use it | yes |
| `crates/testing` | anything; only as a dev-dependency | yes |
| `apps/core` | `protocol`, `model`, `client`, `agents`, `codemap`, `observability`, `appkit`, `version` | yes |
| `apps/server` | `protocol`, `model`, `observability`, `appkit`, `version`; never `agents`, because the server never runs agents | yes |
| `tooling/*` (Rust tools) | nothing in the repo | yes |
| `tooling/ts` | nothing in the repo; reads `crates/protocol`'s schema, `Cargo.toml` and git | yes |
| `apps/web` | TypeScript and React, built into the server's binary; only the types generated from `protocol`, and `tooling/ts` | — |
| `apps/extension` | TypeScript; only the types generated from `protocol`, and `tooling/ts` | — |
| `integrations/` | no code: the plugin files, hooks and skills Weacon gives people's agents, one folder per vendor; only `apps/core` reads them, built into its binary | — |

- Apps never depend on other apps, except that the server embeds the web app's built files. The web app and the core reach the server only through its API, defined in `protocol`; the extension reaches the core only through native messaging.
- Besides its API and MCP endpoint, the server serves only the web app's static files: no server-rendered pages.
- Every app depends on `observability`, and on `version` both as a dependency and as a build-dependency whose `emit()` its `build.rs` calls.
- A crate without I/O never touches `std::fs`, `std::net`, `std::process`, `std::env` or an async runtime; it takes what it needs as arguments.
- Only the `db` module (`src/db.rs` or `src/db/`) of `apps/core` and `apps/server` uses a database driver, and only those apps may depend on one. The database is SQLite.
- Only a `platform` module (`src/platform.rs` or `src/platform/`) selects an operating system with `cfg`; in the core that is `apps/core/src/platform`.
- Only `crates/agents` starts agent processes.
- `integrations/` holds only JSON and Markdown. Its text reaches people's agents, so it follows the UI copy rules and names no real project.
- Only `crates/observability` depends on Sentry's crates.
- A source file holds at most 400 lines before its test module, and no TODO or FIXME: finish the work, or leave the note out.

## Code style

- **Original code only:** never copied, renamed or translated from another project.
- **Simple, clear, terse.** Write the shortest code that reads in one pass, never the cleverest: `?` and early returns, `let ... else`, `if let ... &&` chains, iterator chains that read top to bottom, `matches!`, field shorthand. Small functions (clippy stops at 50 lines, 5 arguments and 4 levels of nesting), plain data, precise types. Owned data and plain structs before trait objects, generics with one implementation, or `Arc<Mutex<_>>`. Nothing built for a need nobody has. `pub(crate)` unless another crate needs it. Modules are `name.rs` beside a `name/` folder, never `mod.rs`.
- **No panics outside tests.** No `unwrap`, `expect`, `panic!` or `unreachable!`; no indexing or string slicing that can panic, so use `get`; no `as` casts, so use `From` and `TryFrom`. Return an error instead. No `unsafe`, except for OS calls with no safe API, inside a `platform` module marked `#[expect(unsafe_code, reason = "...")]`, each block with a `// SAFETY:` comment.
- **Suppressions** are `#[expect(lint, reason = "...")]`, never `#[allow]`, so a stale one fails the build.
- **Edges and insides.** Parse data with `protocol` types where it enters a process; inside, trust the types.
- **Side effects as arguments.** Crates take clocks, id generators, file systems and process spawning as arguments; only apps wire in the real ones.
- **Async.** tokio, in the apps and the crates that do I/O. Never block inside async code (use `spawn_blocking`), never hold a lock across `.await`, and keep every `select!` branch safe to cancel.
- **Comments are few, short and to the point.** Write one only where a reader would otherwise get it wrong: a constraint not visible locally, a workaround for someone else's behaviour, a line that looks like a mistake and isn't. Never narrate the next line, restate a name, list what the code or its tests already show, tell history, re-argue a decision or cite a document. A comment is one line, two at most; a doc comment states the contract only where the signature doesn't, plus the sections and examples clippy and doctests need. Prefer a better name to a comment.
- **Async results:** a stale result never overwrites a newer one; give each request a number and drop answers to old ones.
- **Refreshes** never clear what they refresh: fetch, then replace; show loading only when nothing is there yet; keep good data when a fetch fails.
- **Command lines:** ids and paths are checked before they reach a command line, so none can be read as an option.
- **Exit codes:** every Rust app ends through appkit's `App::finish`, which logs `app stopped` or `app failed` and exits 0 on success, 1 when a check finds something (such as `map check-anchors` finding stale pins), and 2 when a command fails, so hooks can tell the two apart. Help, the version and usage errors answer before the app starts, so they log nothing.
- **App wiring is shared.** Startup, the root logger, log files, error reporting, signals, the runtime and the exit live in `crates/appkit`, their pure parts in `crates/observability`. A second app uses them and never copies the first one's. On people's devices a `Reporter` sends nothing until the person has agreed, so the core starts with agreement off; the server starts with it on, because its operator set the DSN.
- **Protocol messages:** one `Message` variant for each role a feature's messages play (a command, an event, a query, an answer), never one per action: actions are a tagged enum inside the body. Each body is a named struct in that feature's module, with a doc line saying who sends it and under which capability. A nested enum ends in `#[serde(other)] Newer`, which `to_json` refuses to send. Ids are token newtypes made with `token_id!`, and a type that holds a secret has a `Debug` that hides it. A new command is `XCommand { action: XAction }` and a new query `XQuery { query: XQueryKind }`, each enum tagged `kind`. An optional field takes `#[serde(default, skip_serializing_if = "Option::is_none")]` and `#[schemars(with = "T")]`, so it is left out, never null.
- **Capabilities:** a program advertises a capability in the same change that implements it; the core's are in its one `CAPABILITIES` list.
- **Subcommands:** one `Command` variant each, `Name(NameCommand)` when it nests, and one `module::run(&app, …)` call in `main.rs`; side effects such as `current_exe` stay inside the module.
- **Background watches,** such as disk space and memory, are a `Watch` with an injected reader, run through `spawn_blocking`. A failed reading warns once until one succeeds, and events start with the area (`disk low`, `memory sampled`).
- **TypeScript apps** share one type generator and one pinned Node version, and take their version and commit by the rules of `crates/version`.
- **Hooks and helpers** that agents run never block them: when Weacon isn't there, they print nothing and exit cleanly.
- **Screenshots and demos** use made-up data, never real project names.
- **Errors** that Weacon raises are a `Fault` from `crates/observability`: a dotted code from `crates/protocol`, written `code!("store.full")` so it is checked when compiled, and a short fixed message with no user content in it; these are the only messages that reach logs. A crate may have its own error enum and convert it into a `Fault` where it is logged or crosses a process boundary.
- **Tests** sit at the end of the file they test, in a `#[cfg(test)] mod tests`, and are deterministic: no sleeps, no fixed ports, no shared state between tests, temporary folders, and per-worktree state in `.dev/`. They test behaviour strongly enough that the mutation tests find no surviving mutant. Temporary folders come from `weacon_testing::TempDir`, recorded logs from its `recording_logger`, and tests that need a whole app build it through one `test_support::app()`, as the core's do. Fakes speak the `protocol` types, never hand-written JSON.
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
- **Where logs go.** The core: rotated JSON lines in its state folder, and stderr in a terminal. CLI commands: warnings and errors on stderr, `--verbose` adds info. The server: stdout, one record per request (method, route template, status, duration, request id). The web app: forwarded to the server. The extension: forwarded to the core.
- **Error reports** use the Sentry protocol, off without a DSN. On people's devices the core sends them only after the person agrees, and Settings can turn them off. The web app forwards its reports to the server, like its logs. `crates/observability` builds and scrubs each report (`Reports::upload`) and the app sends it through `crates/appkit`, which only apps use; report errors with `capture_error` and a `code` tag, never messages. A DSN in `WEACON_SENTRY_DSN` turns them on. The release is `version!().release()`.
- **Versions** come only from `weacon_version::version!()`: `weacon --version`, `GET /version` on the server (`{"name", "version", "commit"}`, `Cache-Control: no-store`), Settings › About in the web app, and the extension's `version_name`.
- **Tests.** Each app has a test that puts marker strings in a message, a token and a file path, triggers a log line and an error report, and checks that no marker reaches either.

## Setup and commands

`scripts/setup.sh` installs everything a fresh machine, cloud session, orb or CI runner needs, and is safe to run again:
- Rust at the version in `rust-toolchain.toml`, through rustup. Where rustup can't download it, as in Claude Code cloud sessions, it uses the installed stable toolchain if that is exactly the pinned version.
- The dependencies, and `llvm-tools` for coverage where it can be downloaded.
- The check tools at pinned versions: cargo-deny, cargo-machete, cargo-llvm-cov and cargo-mutants, built from crates.io the first time.
- The extension's and the web app's npm packages, at the exact versions in their `package-lock.json`, and the Chromium the web app's end-to-end tests drive, unless it is already in `PLAYWRIGHT_BROWSERS_PATH`. It needs the Node in `.node-version`, which every TypeScript app and `tooling/ts` share, and stops if another is installed.
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
| `scripts/check.sh` | What CI runs: rustfmt, clippy with warnings as errors, docs, cargo-machete, cargo-deny, all tests with the 90% line-coverage floor, doc tests, mutation tests on the code changed against `origin/main` (in CI each test binary gets at most 4 GiB of memory), the extension's type check, tests and build, the shared TypeScript tooling's tests, the web app's type check, build, tests and end-to-end tests in Chromium against `fake-server`, and ShellCheck. CI also lints and tests on Windows |
| `cargo test -p <crate> [name]` | Focused tests while iterating |
| `WEACON_STATE_DIR=.dev/state cargo run -p weacon-core -- doctor` | Runs the core with this worktree's own state folder |
| `cargo run -p weacon-core -- map [symbol <path>] [--root <folder>] [--budget <bytes>]` | Prints where to begin in a repository's code, or where one item is, exactly as the local MCP's `map` and `symbol` tools answer |
| `UPDATE_SCHEMA=1 cargo test -p weacon-protocol` | Regenerates `crates/protocol/schema.json` after a message changes |
| `UPDATE_VECTORS=1 cargo test --locked -p weacon-core seal` | Regenerates the sign-in terminal's Noise vectors in `crates/protocol/noise-vectors.json` after its messages change |
| `npm --prefix apps/extension run update-types`, `npm --prefix apps/web run update-types` | Regenerates each app's `src/protocol.ts` from `schema.json` |
| `npm --prefix apps/web run build` | Builds the web app into `apps/web/dist/` (`index.html`, `app.js`, `app.css`), with this commit in Settings › About |
| `npm --prefix apps/web run measure` | Streams the 100,000-token session into the built web app in Chromium, then scrolls the 5,000-line diff, and prints frame gaps, memory and bundle size |
| `mkdir -p .dev/fake && cargo run --locked -p weacon-testing --bin fake-server -- .dev/fake`, then open `apps/web/dist/index.html?server=<the address it prints>` | Streams a made-up 100,000-token session to the web app |
| `npm --prefix apps/extension run build` | Builds the extension into `apps/extension/dist/`, with this commit in its manifest's `version_name`; load that folder unpacked in Chrome, then run `weacon connect-chrome` |
| `scripts/release.sh <target>` | Builds a static release `weacon` for one target into `dist/`, checks it reports this commit, and prints its size |
| `scripts/release.sh server <target>` | Builds the web app, then a static release `weacon-server` with it inside for a Linux target into `dist/`, checks it reports this commit and serves this web app, and prints its size |
| `scripts/release.sh install <[user@]host> <target>` | Copies that Linux build to a host over SSH and runs `weacon service install` there: a systemd user service, with lingering on |
| `scripts/release.sh manifest <tag> <folder> <https base URL>` | Writes the unsigned `manifest.json` for the core's builds in a folder: version, commit, protocol, and each target's URL, size and SHA-256. The release workflow runs it for a tag and uploads it with the builds to a draft release |
| `scripts/release.sh sign <tag> <secret key file>` | Run by the owner, on the machine with the release key: checks the draft's manifest against its tag (version, commit, every target, each URL) and each build against its SHA-256, dates and signs the manifest with minisign, checks the signature against the keys the tag's core trusts, and uploads both to the draft |

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

- **One task, one branch, one worktree.** Name branches `<agent>/<topic>` (`claude/core-skeleton`, `codex/protocol-framing`, `amp/web-app`). Start from a fresh `origin/main` and rebase on it before committing.
- **Open a draft PR early,** as soon as the first commit exists.
- **Stay in your lane.** A task owns the folders it names. Touching others, this file, root configs or `tooling/archtest/src/rules.rs` goes in its own small PR.
- **Small PRs.** Each should be reviewable in a few minutes. The owner squash-merges.
- **No shared append-only files.** No changelog. Generated files are regenerated after a conflict, never merged by hand; for `Cargo.lock`, take `main`'s and run cargo again.
- **Nothing fixed per machine.** Random ports, temporary folders, caches under the user's cache directories, and per-worktree state in `.dev/`; each worktree builds into its own `target/`.
