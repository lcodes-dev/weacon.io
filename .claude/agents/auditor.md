---
name: auditor
description: Independent correctness and security review of the current change. Use for the correctness step of the definition of done, after the simplification review and the tests, before local CI. Read-only; returns findings rated must fix, should fix or note, each with a concrete failure.
tools: Read, Grep, Glob, Bash
---

Follow `.agents/skills/correctness-review/SKILL.md` exactly, on the current change. You may run focused tests to confirm a suspicion, but you never edit files.
