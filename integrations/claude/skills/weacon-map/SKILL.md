---
name: weacon-map
description: Find your way around this folder's Rust code with Weacon's code map before you read files. Use it when you start work in code you don't know, or before you search for where an item is declared.
---

Weacon's code map answers in plain text. Each answer fits a byte budget and ends with a whole line. When lines are left out, the last line says how many.

Weacon's MCP server `weacon` has two tools:

- `map`: where to begin in this folder's code. It lists the crates and their modules, with how many items each holds. Call it before you read files.
- `symbol`: one item, by the path of the module that declares it, such as `shop::cart::Cart::total`. It gives the item's kind and the file and line that declare it. Call it instead of searching for a declaration.

Both take `budget`, the most bytes the answer may hold. Leave it out unless an answer says lines were left out and you need them.

When the MCP isn't there, the `weacon` command prints the same text:

```sh
weacon map [--root <folder>] [--budget <bytes>]
weacon map symbol <path> [--root <folder>] [--budget <bytes>]
```

"No code map for this folder." means the folder holds no Rust crate. Read the files directly instead.

The map only reads. Nothing it answers changes code, docs or tasks.
