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
8. **Should registering create a personal team already in plan 02?** You described accounts with their own team, as Laravel's starter kits do. Plan 02 creates that team at registration and shows its name in the sidebar; invitations, switching teams and projects wait for later plans. The alternative is to leave teams out of plan 02 entirely. *(plan 02; built with "create it" unless you say otherwise)*
9. **Is an SMTP password stored as plain text acceptable for now?** Plan 02 keeps it in the `config` table as it is; the alternative is encrypting it with a server key. *(plan 02; recommended: yes, noted in improvements)*

## Answered

- **Email codes or passwords?** Codes. *(2026-11-17)*
- **Who may claim a fresh server?** Whoever registers first; an open server is fine. *(2026-11-17)*
- **Is one team per server right?** No. The owner is only the server's owner. Anyone can register and gets an account with their own team, or is invited to a team. Each team holds several projects, as Laravel's starter kits do. The word is always "team". *(2026-11-17)*
- **How should SMTP be configured and checked?** The owner saves the SMTP settings in a `config` table; the server reads them and keeps them in memory. With no SMTP settings, mail is printed to the log. *(2026-11-17)*
