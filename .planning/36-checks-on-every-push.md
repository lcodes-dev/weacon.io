# Plan 36: checks on every push

**Status: done** (2026-11-25)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: every push is formatted, linted, built and tested by GitHub, so that a change that breaks something says so within minutes. Today nothing runs unless whoever made the change runs it, and changes land on `main` directly.

## One script

`scripts/check.sh` runs everything, in the order that fails soonest:

1. `cargo fmt --check`
2. `npm ci` and the web app's build, which type-checks it and which the server's tests need
3. `cargo clippy --all-targets -- -D warnings`
4. `cargo test --workspace`

A person runs it before a push; GitHub runs the same script. So the two can't check different things.

- **The queries are checked by the build itself,** against the committed `.sqlx/`: a query changed without `scripts/prepare-queries.sh` fails to compile. The script sets `SQLX_OFFLINE` so that it never looks for a database.
- **No linter is added for the web app.** Its build type-checks it, and nothing else checks it today.

## On GitHub

One workflow, `.github/workflows/check.yml`, on every push to `main` and every pull request:

- on Linux, with Node and Rust at the versions in `mise.toml`, installed by mise's own action; `sqlx-cli` is left out, since the script doesn't need it and it takes minutes to build;
- with the build's output and the docs search's model cached between runs, since a first build takes many minutes and the model is 30 MB;
- one run at a time for a branch, a newer push stopping the older run.

A red run is mailed by GitHub to who pushed. Nothing blocks a push to `main`: the check reports, it doesn't gate.

## What may need a fix first

The tests have only run on macOS. The first run on Linux will say whether any of them depends on it: the agents' stand-in scripts, a chat's copy of a repository with the runner's git, and the docs search's model. What it finds is fixed in this plan.

## Where it goes

| Where | What |
| --- | --- |
| `scripts/check.sh` | The four checks |
| `.github/workflows/check.yml` | The script, on a push and a pull request |
| `AGENTS.md`, `README.md` | The script's row among the commands |

## Not in this plan

- Building the server and the core for release, and macOS or Windows runners.
- A rule on `main` that requires the check, or pull requests for every change.
- Checking the packages for advisories.
- The real agents, which need a sign-in.

## Steps

- [x] `scripts/check.sh`, passing on this machine
- [x] The workflow, and its first green run on GitHub, with whatever Linux needed fixed
- [x] `AGENTS.md` and `README.md`

## Outcome

Built as planned, with one fix for Linux:

- **`mise.toml` names `rustfmt` and `clippy`** beside the Rust version, since mise installs Rust on GitHub without them. No test needed a change.
- **`sqlx-cli` is left out on GitHub** by mise's own setting for tools to ignore, in the workflow.
- **The first green run took under three minutes** from an empty cache, with the same 96 tests passing as on macOS.
- **Questions 75 and 76 were built with their defaults:** Linux only, and a check that reports without gating `main`.
