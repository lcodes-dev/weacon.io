# Open questions

Questions waiting for the owner's answer. When one is answered, move it under "Answered" with the answer and the date.

## Open

1. **Is the new `AGENTS.md` what you want?** UI SDK's setup needed a note there, so a short one was written: the simplicity rule, what `.legacy/` and `.planning/` are, the commands, and UI SDK's note. Keep it, or replace it with rules you write yourself? *(from plan 01; built with the default "keep it")*
2. **Should the shell show placeholder items?** The sidebar has its headings only. The alternative is the reference's sample chats and machines as placeholder data until real ones exist. *(from plan 01; built with the default "empty")*
3. **Are ports 8080 (server) and 5173 (web app in development) fine?** They were picked without discussion.
4. **How should changes land?** Nothing is committed yet: the move to `.legacy/` is staged on `main` and the new project is untracked. The old rule was a branch and a draft PR for every change, reviewed by CodeRabbit; that rule moved to `.legacy/` with the old `AGENTS.md`. Commit to `main` directly, or keep branches and PRs?
5. **Should `LICENSE` come back to the root?** It moved to `.legacy/` with everything else, so the new project has no licence file.
6. **Keep `.claude/launch.json`?** It wasn't in the plan; it lets Claude's browser pane start `scripts/dev.sh`.
7. **Keep UI SDK's skill and `skills-lock.json` in the repository?** Its installer added `.agents/skills/ui-sdk` (linked from `.claude/skills`) and the lock file at the root.

## Answered

None yet.
