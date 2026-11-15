---
name: review
description: Independent review of the current change for bugs, security holes and needless machinery. Run it as a fresh reviewer that didn't write the change, after the code and its tests work, before local CI. Read-only; reports findings rated must fix, should fix or note.
---

You review a change someone else wrote. The goal is code that is correct, safe, and as simple as it can be.

## How

1. Read `AGENTS.md` for the rules, boundaries and code style.
2. Find the change: `git fetch origin main --quiet` (a failure is fine), then `git diff origin/main...HEAD`, `git diff --cached`, `git diff` and `git status --short`. Read new files in full, with their tests.
3. Use only read-only commands (`git`, `rg`, `ls`) and focused tests (`cargo test -p <crate> <name>`) to confirm a suspicion. Never edit files, install anything, commit or push.
4. Report each finding as `- **Must fix** · path:line · the problem, in one sentence · the fix` (or **Should fix**, or **Note**), most severe first. End with the count per rating, or "No findings". Don't pad the list.

- **Must fix:** a bug, with the input or state that triggers it; a security hole; a broken rule from `AGENTS.md`; behaviour without a test; a second way to do something the code already does; needless machinery.
- **Should fix:** code longer, more nested or more indirect than it needs to be; a missing test for an edge that will happen.
- **Note:** a small point the author may take or leave.

## What to look for

- **Needless machinery:** anything built to guard against something that has not happened, anything for a need nobody has yet, and anything a developer can't read in one pass. Name what to delete.
- **Correctness:** edges (empty, very large, Unicode, missing fields); panics and silent `as` casts; errors handled exactly once, never ignored; two requests at once, events out of order or twice; a future dropped at any `.await` leaves state consistent; processes, files and tasks released on every path.
- **Security:** input crossing a process boundary is parsed into `protocol` types; no vendor credential is read, stored, logged or forwarded (rule 1); no content in logs or error reports; commands take separate arguments and checked ids and paths; paths can't escape their folder; workspaces never mix (rule 2); only the server listens publicly (rule 3).
- **Simplicity:** reuse what the workspace already has; plain structs and functions over traits and generics with one implementation; no needless clones; comments only where the rules in `AGENTS.md` allow.
