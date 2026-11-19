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
11. **Who may create, edit and delete projects?** Plan 05 lets any member create and edit, and only the team's owner delete. *(plan 05)*
12. **Is GitHub the only host for now?** Plan 05 connects to GitHub only. *(plan 05; recommended: yes)*
13. **Can a repository only be picked from the ones the team's app can reach?** Plan 05 says yes, so a team with no GitHub App can only make projects without a repository. The alternative also lets people type `owner/name` unchecked. *(plan 05; recommended: yes)*
14. **Is one GitHub App per team right?** In plan 05 each team's owner registers a private app for their team. The alternative is one app for the whole server, which needs a step by the server's owner and a GitHub sign-in for each person. *(plan 05; recommended: per team)*
15. **Is one GitHub account per team enough?** A private app can only be installed on the account that owns it, so a team's projects all come from one user or one organization. *(plan 05; recommended: yes for now)*
16. **Should the map wait for a webhook?** Plan 06 asks GitHub once a minute and keeps a queue a webhook can later write to. A webhook now would need a public address, a secret and signature check, and every team's GitHub App registered again. *(plan 06; recommended: ask now, webhook later)*
18. **Is "uses" by matching names good enough to start?** Plan 06 links file A to file B when A mentions a name only B defines. Links that understand each language are far more work. *(plan 06; recommended: yes)*
19. **Is running `claude` or `codex` on the server's own computer the right way to use a subscription for now?** Plan 07 does that, for the server's owner only, until machines exist. *(plan 07)*
20. **May the server keep the text of every file of a repository in its database?** Plan 07 does, so docs never need a second download. The database grows by about the size of the repository's text, once per distinct content. *(plan 07; recommended: yes)*
21. **Is a run of up to about 750,000 tokens acceptable?** That is up to 16 pages of up to 150,000 characters each, plus the outline; flows are usually much smaller. On a subscription it comes out of that account's usage limits. *(plan 07)*
22. **Should the writer decide which folders aren't the project's own code?** Plan 07 lets it, and shows what it left out. The alternative is a field on the project where a person lists them. *(plan 07; recommended: let it, add the field if it gets it wrong)*
23. **Does the overview belong with the technical docs?** Plan 14 puts it there, since it describes how the code is laid out, so a public link shows only the "How it works" pages. *(plan 14; built with the default "yes")*
24. **May any member share the user docs and end a link?** Plan 14 says yes. The alternative is the team's owner only. *(plan 14; built with the default "any member")*
25. **Is a person's edit saved at once, with only the agent's changes needing approval?** Plan 15 is built on that. The alternative also sends a person's edits through approval by someone else. *(plan 15; recommended: yes)*
26. **Is one request and one proposal enough until chats exist?** Plan 15 has no back-and-forth with the agent on docs; a rejected request can be asked again with the earlier one as context. *(plan 15; recommended: yes)*
27. **Is a Markdown text box with a preview the right editor to start with?** The alternative hides the Markdown behind a rich editor, which is a large dependency. *(plan 15; recommended: yes)*
28. **May images live in the database, up to 5 MB each?** It keeps a backup to one file. The alternative is a folder beside the database. *(plan 15; recommended: yes)*
29. **Should "Check against the code" run only when asked?** Plan 15 runs it inside every run and request, and alone only on a click, since each check is a call to the writer. *(plan 15; recommended: yes)*
