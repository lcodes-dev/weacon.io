---
name: correctness-review
description: Independent correctness and security pass on the current change. Run it as a fresh reviewer that didn't write the change, after the simplification review and the tests, before local CI. Read-only; reports findings rated must fix, should fix or note, each with a concrete failure.
---

You review a change someone else wrote, looking for bugs and security holes.

## How

1. Read `AGENTS.md` for the rules this pass checks.
2. Find the change: `git fetch origin main --quiet` (a failure is fine), then `git diff origin/main...HEAD`, `git diff --cached`, `git diff` and `git status --short`. Read new files in full, with their tests.
3. Use only read-only commands (`git`, `rg`, `ls`) and focused tests (`cargo test -p <crate> <name>`) to confirm a suspicion. Never edit files, install anything, commit or push.
4. For each finding, work out the concrete failure: the input or state, and what goes wrong. A finding without one is a note at most.
5. Report each finding as `- **Must fix** · path:line · the failure, in one sentence · the fix` (or **Should fix**, or **Note**), grouped under "Correctness" and "Security", most severe first. End with the count per rating, or "No findings". Don't pad the list.

- **Must fix:** a bug, a security hole, a broken rule from `AGENTS.md`, or behaviour without a test.
- **Should fix:** a likely failure under conditions that will happen soon, or a missing test for an edge case.
- **Note:** a hardening idea the author may take or leave.

## Correctness

- **Edges:** empty input, one item, very large input, Unicode and emoji, unusual whitespace, missing optional fields. Slicing a `str` at a byte offset that isn't a character boundary panics; an `as` cast truncates or wraps silently where `try_from` would fail.
- **Panics:** no `unwrap`, `expect`, `panic!`, `unreachable!`, failing index or overflowing arithmetic on a path that input from outside the crate can reach. A panic inside a tokio task ends only that task, and its error is easy to lose.
- **Errors:** is every failure handled exactly once, with a stable error code, and either recovered from or reported? No ignored `Result` (`let _ =`, `.ok()`), no dropped `JoinHandle` whose error matters, no error mapped to a vaguer one that hides a bug.
- **Concurrency and order:** two requests at once, events arriving out of order or twice, a retry after a timeout. Are writes idempotent where they can be retried? No `std::sync` lock held across an `.await`; no blocking call (file or process I/O, SQLite, heavy CPU) on the async runtime outside `spawn_blocking`.
- **Cancellation:** a future can be dropped at any `.await`, by `select!`, a timeout or a closed connection. Does that leave state consistent? Work that must finish runs in its own task or a transaction.
- **Resources:** are child processes killed and waited for, and sockets, files, tasks and timers released on every path, including errors and cancellation?
- **State:** can the code reach a state that shouldn't exist? Is there one state machine for each thing?
- **Time:** `Instant` for durations and timeouts, wall-clock time only to show or store; time zones handled; no reliance on wall-clock order.
- **Tests:** does a test fail without the change? Are the failure paths and edges above tested, deterministically? Do the mutation tests in `scripts/check.sh` find every mutant?

## Security

- **Edges:** is everything that crosses a process boundary (socket, named pipe, HTTP, native messaging, MCP, files written by other programs) deserialized into `protocol` types before use, with its size bounded before it's read?
- **Sign-ins:** does the change read, store, log, copy or forward any agent or vendor credential? It must not; it may only read sign-in status. The one exception is rule 1's sign-in terminal: only the vendor's login command, with fixed arguments and no shell, sealed end to end with the core's key checked against the pairing fingerprint, and never parsed, stored or logged.
- **Content in logs and reports:** can messages, prompts, agent output, file contents, diffs, tokens or cookies reach a log line or an error report? Can they leak through a `Debug` or `Display` impl? Is there a marker-string test that proves they can't?
- **Commands:** processes are spawned with `Command` and separate arguments, never `sh -c` with a built string; ids and paths are validated, and user-controlled values can't be read as options (put them after `--`).
- **Paths:** paths are resolved and checked against the folder they must stay in; symlinks can't escape it; no check-then-use race on a file (open it once, then work with the handle; create with `create_new`).
- **Unsafe code:** none, unless the platform layer needs it for an OS call; then each block says why it's sound and has a test.
- **Network:** only the server listens publicly, and its routes check the session or token, the membership in the workspace on every request, and the origin. The core only dials out and verifies the server's TLS; its local servers bind to 127.0.0.1 and check a token.
- **Tenancy:** every row and query carries the workspace; no data can cross workspaces; every query goes through the `db` module's workspace handle, and foreign keys include the workspace id. Only a person's account and what belongs to them alone on the server, their sessions, email sign-in challenges and paired machines, are server-wide, and they hold no workspace content. A membership carries its workspace id and refers to its account by id; the `db` module's account handle reaches only that account's own rows and lists its memberships, and everything in a workspace goes through the workspace handle. Membership is checked on every request, so removing it ends access at once, and nothing one workspace sees reveals another or whether an account exists.
- **Web app and extension:** the web app never renders agent output or user content as raw HTML, keeps a strict content security policy, and only the browser and the core hold the keys of a sealed terminal. The extension's permissions are minimal, its content security policy is strict, and its messages are checked for their sender.
- **Dependencies:** each new crate is needed, maintained, under a licence `.cargo/deny.toml` allows; its build script and procedural macros are worth the trust they need. The extension's packages run no install scripts unless approved.
