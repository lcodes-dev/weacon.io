# Open questions

Questions waiting for the owner's answer. When one is answered, delete it from this file; the answer lives in the plan, the docs or the code it changed.

1. **Is the new `AGENTS.md` what you want?** UI SDK's setup needed a note there, so a short one was written: the simplicity rule, what `.legacy/` and `.planning/` are, the commands, and UI SDK's note. Keep it, or replace it with rules you write yourself? *(from plan 01; built with the default "keep it")*
2. **Should the shell show placeholder items?** The sidebar has its headings only. The alternative is the reference's sample chats and machines as placeholder data until real ones exist. *(from plan 01; built with the default "empty")*
3. **Are ports 8080 (server) and 5173 (web app in development) fine?** They were picked without discussion.
5. **Should `LICENSE` come back to the root?** It moved to `.legacy/` with everything else, so the new project has no licence file.
6. **Keep `.claude/launch.json`?** It wasn't in the plan; it lets Claude's browser pane start `scripts/dev.sh`.
7. **Keep UI SDK's skill and `skills-lock.json` in the repository?** Its installer added `.agents/skills/ui-sdk` (linked from `.claude/skills`) and the lock file at the root.
9. **What should the mail footer say?** Plan 03 uses "Sent by Weacon. If you didn't ask for this mail, ignore it." on every mail. *(plan 03)*
10. **How long should an invite work?** Plan 04 uses 7 days, renewed by "Resend". *(plan 04)*
11. **Who may create, edit and delete projects?** Plan 05 lets any member create and edit, and only the team's owner delete. *(plan 05)*
12. **Is GitHub the only host for now?** Plan 05 connects to GitHub only. *(plan 05; recommended: yes)*
13. **Can a repository only be picked from the ones the team's app can reach?** Plan 05 says yes, so a team with no GitHub App can only make projects without a repository. The alternative also lets people type `owner/name` unchecked. *(plan 05; recommended: yes)*
14. **Is one GitHub App per team right?** In plan 05 each team's owner registers a private app for their team. The alternative is one app for the whole server, which needs a step by the server's owner and a GitHub sign-in for each person. *(plan 05; recommended: per team)*
15. **Is one GitHub account per team enough?** A private app can only be installed on the account that owns it, so a team's projects all come from one user or one organization. *(plan 05; recommended: yes for now)*
16. **Should the map wait for a webhook?** Plan 06 asks GitHub once a minute and keeps a queue a webhook can later write to. A webhook now would need a public address, a secret and signature check, and every team's GitHub App registered again. *(plan 06; recommended: ask now, webhook later)*
19. **Is running `claude` or `codex` on the server's own computer the right way to use a subscription for now?** Plan 07 does that, for the server's owner only, until machines exist. *(plan 07)*
20. **May the server keep the text of every file of a repository in its database?** Plan 07 does, so docs never need a second download. The database grows by about the size of the repository's text, once per distinct content. *(plan 07; recommended: yes)*
21. **Is a run of up to about 750,000 tokens acceptable?** That is up to 16 pages of up to 150,000 characters each, plus the outline; flows are usually much smaller. On a subscription it comes out of that account's usage limits. *(plan 07)*
22. **Should the writer decide which folders aren't the project's own code?** Plan 07 lets it, and shows what it left out. The alternative is a field on the project where a person lists them. *(plan 07; recommended: let it, add the field if it gets it wrong)*
23. **Does the overview belong with the technical docs?** Plan 14 puts it there, since it describes how the code is laid out, so a public link shows only the "How it works" pages. *(plan 14; built with the default "yes")*
29. **How long should the server wait for a machine that went away in the middle of a run?** Plan 19 says ten minutes, then the run fails. A laptop that sleeps for an hour would lose a docs run; a chat will need a better answer than a timeout. *(plan 19; recommended: ten minutes for docs runs)*
32. **Should the core install a missing agent?** Plan 19 offers the vendor's own installer and runs it only on a yes. The alternatives are only saying how, or installing without asking. *(plan 19; recommended: offer and ask)*
33. **What should a run do when its agent isn't signed in?** Plan 19 stops the run with a message and a link to the sign-in; the person starts the run again. It could instead wait some minutes for the sign-in and go on by itself. *(plan 19; recommended: stop and say so)*
54. **What of a chat may reach the server?** Today it gets the agent's words, the commands it ran without their output, and the whole diff, and teammates read all three. The project's spec left this open, and plans 21 and 22 answered it by building. On a server someone else hosts, that is the person's code. *(plans 21 and 22; recommended: keep it, and decide again before a hosted server)*
72. **May `@weacon` propose changes to the docs too, or only a person's own agent?** Plan 34 gives the tool to both, since they share one list of tools. `@weacon` hasn't read the code, so its proposals come from what was said in a group. *(plan 34; built with the default "both")*
73. **Should a docs run keep the agents' proposals nobody answered?** Today a run drops every unanswered proposal. Plan 34 has it drop only the writer's own. *(plan 34; built with the default "keep them")*
74. **Do all the timers go, or only the fast ones?** Plan 35 removes all ten, in two steps: chats and groups first, then tasks, docs, projects and machines. Stopping after the first step leaves those four asking every few seconds. *(plan 35; built with the default "all")*
75. **Is Linux alone enough for the checks?** Plan 36 runs them on Linux only. No test differs by system today, and a second system doubles what a run waits for. *(plan 36; built with the default "Linux only")*
76. **Should the check gate `main`?** Plan 36 only reports: a red run is mailed, and nothing blocks a push. Gating needs a pull request for every change. *(plan 36; built with the default "report only")*
