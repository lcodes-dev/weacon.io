---
name: weacon-map
description: Find your way around a Rust repository with Weacon's code map before reading files - where to begin, and where an item is declared. Use when starting work in unfamiliar code, or before opening files to find an item.
---

Weacon's code map answers in plain text, within a byte budget, and each answer ends with a whole line. When it leaves lines out, its last line says how many.

**In a run Weacon started**, its MCP server `weacon` offers two tools:

- `map`: where to begin in the folder's code, meaning its crates and their modules, with how many items each holds. Call it first, before reading files.
- `symbol`: one item by its full path, such as `weacon_protocol::frame::Envelope`. It gives the item's kind and the file and line that declare it. Call it instead of searching for a declaration.

Both take `budget`, the most bytes the answer may hold: 6000 unless given, kept between 1000 and 16000. Ask for more only when an answer says lines were left out and you need them.

**Without the MCP**, for example in a Codex run, or in a terminal, the `weacon` command prints the same text:

```sh
weacon map [--root <folder>] [--budget <bytes>]
weacon map symbol <path> [--root <folder>] [--budget <bytes>]
```

`--root` is the repository's root folder, the current folder unless given.

"No code map for this folder." means the folder holds no Rust crate Weacon can read. Read the files directly instead.

The map only reads. Nothing it answers changes code, docs or tasks.
