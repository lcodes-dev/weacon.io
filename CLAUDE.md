@AGENTS.md

## Claude Code

- Run the definition of done with the `finish` skill (`/finish`). Its review uses the `reviewer` subagent in `.claude/agents/`, so it isn't anchored on the code's author.
- GitHub's GraphQL API isn't available in cloud sessions, so `gh pr` commands fail there. Use `gh api` with REST paths, as the `finish` skill shows, and the GitHub tools to mark a PR ready, reply to findings and resolve threads.
