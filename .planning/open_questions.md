# Open questions

Questions waiting for the owner's answer. When one is answered, delete it from this file; the answer lives in the plan, the docs or the code it changed.

1. **Is the new `AGENTS.md` what you want?** UI SDK's setup needed a note there, so a short one was written: the simplicity rule, what `.legacy/` and `.planning/` are, the commands, and UI SDK's note. Keep it, or replace it with rules you write yourself? *(from plan 01; built with the default "keep it")*
2. **Should the shell show placeholder items?** The sidebar has its headings only. The alternative is the reference's sample chats and machines as placeholder data until real ones exist. *(from plan 01; built with the default "empty")*
3. **Are ports 8080 (server) and 5173 (web app in development) fine?** They were picked without discussion.
4. **How should changes land?** Nothing is committed yet: the move to `.legacy/` is staged on `main` and the new project is untracked. The old rule was a branch and a draft PR for every change, reviewed by CodeRabbit; that rule moved to `.legacy/` with the old `AGENTS.md`. Commit to `main` directly, or keep branches and PRs?
5. **Should `LICENSE` come back to the root?** It moved to `.legacy/` with everything else, so the new project has no licence file.
6. **Keep `.claude/launch.json`?** It wasn't in the plan; it lets Claude's browser pane start `scripts/dev.sh`.
7. **Keep UI SDK's skill and `skills-lock.json` in the repository?** Its installer added `.agents/skills/ui-sdk` (linked from `.claude/skills`) and the lock file at the root.
8. **Is an SMTP password stored as plain text acceptable for now?** Plan 02 keeps it in the `config` table as it is; the alternative is encrypting it with a server key. *(plan 02; recommended: yes, noted in improvements)*
9. **What should the mail footer say?** Plan 03 uses "Sent by Weacon. If you didn't ask for this mail, ignore it." on every mail. *(plan 03)*
10. **How long should an invite work?** Plan 04 uses 7 days, renewed by "Resend". *(plan 04)*
