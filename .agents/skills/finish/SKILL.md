---
name: finish
description: Run Weacon's definition of done on the current change - simplify, test, check correctness, run local CI, commit and open a draft PR. Use whenever a piece of work is ready to commit.
---

Walk through the definition of done in `AGENTS.md`, in order. Fix anything that fails before moving on. How to do each step:

1. **Code and tests.** List the behaviour the change adds or changes and the test that covers each item; write the missing ones. A bug fix needs a test that failed before the fix.
2. **Simplification review.** Start a fresh reviewer as `AGENTS.md` says for your agent, with no hint of what you expect, on `.agents/skills/simplify-review`. Fix every *must fix*; fix each *should fix* or note why not.
3. **Tests.** `cargo test -p <crate>` for each crate you touched.
4. **Correctness and security review.** The same way, on `.agents/skills/correctness-review`, on the simplified code. Fix every *must fix* with a test; fix each *should fix* or note why not. Rerun the tests.
5. **Local CI.** `scripts/fix.sh`, then `scripts/check.sh`, until it passes in full, mutation tests included. Fix the cause, never the check. If a fix changed behaviour, go back to step 2.
6. **Rules.** If a rule, a boundary or a command changed, `AGENTS.md` says so in this change.
7. **Commit.** Stage only this change's files, with the message `AGENTS.md` step 7 describes.
8. **Draft PR.** `git push -u origin HEAD`, then open a draft PR against `main`, or update the open one. GraphQL isn't available in cloud sessions, so use the REST API:

   ```bash
   gh api 'repos/{owner}/{repo}/pulls' -f base=main -f head="$(git branch --show-current)" \
     -f title="<subject>" -F body=@.dev/pr-body.md -F draft=true
   ```

   To update one, `gh api -X PATCH 'repos/{owner}/{repo}/pulls/<number>' -F body=@.dev/pr-body.md`. Never mark it ready or merge it.

Finish by saying, in a few lines, what was committed, the PR link, and anything left open.
