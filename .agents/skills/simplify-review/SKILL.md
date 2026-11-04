---
name: simplify-review
description: Independent simplification pass on the current change, at three levels (the file, the feature, the project). Run it as a fresh reviewer that didn't write the change, after the code works and before the tests and the correctness review. Read-only; reports findings rated must fix, should fix or note.
---

You review a change someone else wrote, looking only for ways to make it simpler. The goal is code as simple, clear and readable as it can be: terse where terseness reads well, never clever, and never built for a need nobody has. rustfmt, clippy and cargo-machete already enforce formatting, many local simplifications, dead code and unused dependencies; this pass covers what they can't.

## How

1. Read `AGENTS.md` for the rules, boundaries and code style.
2. Find the change: `git fetch origin main --quiet` (a failure is fine), then `git diff origin/main...HEAD`, `git diff --cached`, `git diff` and `git status --short`. Read new files in full.
3. Use only read-only commands (`git`, `rg`, `ls`). Never edit files, install anything, commit or push.
4. Go through the three levels below in order; each looks at a wider context, so a finding at a wider level can make a narrower one moot.
5. Report each finding as `- **Must fix** · path:line · what could be simpler, in one sentence · the simpler version` (or **Should fix**, or **Note**), grouped under "The file", "The feature" and "The project", most severe first. End with the count per rating, or "No findings". Don't pad the list.

- **Must fix:** a second way to do something the code already does, dead or speculative code, an abstraction that makes the change harder to follow, or a comment the rules in `AGENTS.md` don't allow.
- **Should fix:** code that is longer, more nested or more indirect than it needs to be.
- **Note:** a small point the author may take or leave.

## 1. The file

- **Shorter:** could a function lose lines, branches, parameters or nesting without losing clarity? `?` and early returns over nested `match`; `let ... else` over a `match` that only unwraps; `if let ... &&` chains over nested `if let`; a `match` or lookup table over a long `if` chain; an iterator chain that reads in one pass over a loop with a mutable accumulator, and a plain loop where a chain would need a comment.
- **Plainer:** could plain structs, enums and functions replace a trait, a generic parameter, a builder or a closure? No `Box<dyn Trait>` or generic with one type behind it. Could the standard library or a crate the workspace already uses replace hand-written code?
- **Ownership:** no needless `.clone()`, `.to_string()`, `.to_owned()` or `.collect()`. Borrow where the callee only reads (`&str`, `&[T]`, `&Path`). No `Arc<Mutex<_>>` where one owner or a channel would do, and no `Rc<RefCell<_>>` to get around the borrow checker.
- **Narrower types:** an enum or a newtype over a `String` or `bool` that only takes a few values; no `Option` field that is always set; no enum variant that never occurs; no `serde_json::Value` where the shape is known.
- **Names:** do names say what things are? Could a better name make a comment unnecessary?
- **Comments:** read every comment and doc comment the change adds or touches; flag any that breaks the comment rule in `AGENTS.md`, or no longer matches the code, and give the shorter version or say to delete it.
- **Nothing dead:** no unused code, commented-out code, `#[allow(dead_code)]`, or "for later" paths.

## 2. The feature

- **Duplication:** is the same logic, check, constant or type written in two files or two crates?
- **Layers:** does a module only re-export or forward calls? Could two modules become one, or one module split along a clearer line?
- **One source of truth:** is each piece of state kept in exactly one place, and each rule decided in exactly one place?
- **Speculation:** a parameter nobody passes, a trait with a single implementation and no planned second, a Cargo feature nobody turns on, a configuration for a value that never changes?
- **Shape:** could the feature be built with fewer files, types or moving parts? Would a different data shape, such as an enum instead of several optional fields, remove code?
- **Tests:** do the tests check behaviour through the crate's public API, or pin private details that make the code harder to simplify later? Is a fake standing in for something a temporary folder or an in-memory database would cover?

## 3. The project

Search before answering: `rg` for similar names, helpers and patterns.

- **Reuse:** does a function, type or crate in the workspace already do this, or nearly? Use it, or extend it slightly.
- **One of each:** a second logger, error type, config path, state machine, ID format, time helper, async runtime or HTTP client? If the existing way is wrong, change it in its own PR first.
- **Placement:** is the code in the crate `AGENTS.md` says it belongs to?
- **Smaller change:** would a smaller change to an existing feature achieve the same result?
- **Consistency:** does it follow the patterns the rest of the code uses for the same problem (errors and their codes, logging, parsing at the edges, tests)?
