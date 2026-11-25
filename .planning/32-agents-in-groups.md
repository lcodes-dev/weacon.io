# Plan 32: calling your agent in a group

**Status: done** (2026-11-25)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: in a group, a person writes `@claude` or `@codex` and their own agent answers there, in front of the others, as the prototype shows. It needs plan 31.

## The rule

**A tag calls the agent of who wrote it, on that person's own machine and sign-in.** Never somebody else's: a subscription is a person's and is used only on their own machines, which is the rule of 20 November. So when Marco writes `@claude`, Marco's Claude Code answers, also when the last answer in the group came from Ada's.

The prototype has one more case, where Marco's tag asks for Ada's agent and Ada approves it first. It is not in this plan. The owner agreed on 25 November that a tag always calls the writer's own agent for now, and that a group gets an agent's last words with the rest in its chat.

## A group's agent is a chat

Nothing new runs an agent. The first time a person tags an agent in a group, Weacon starts a chat of theirs with that agent, tied to the group; each later tag is that chat's next turn. So everything a chat has is there: the copy of the repository on the person's machine, the sandbox, the hosts, the tools for tasks and docs, the diff and the pull request.

- **One chat for each person and agent in a group.** It works on the group's project, from that project's first mapped branch, on the person's machine that has the agent signed in, a connected one before one that is away.
- **What the agent is told.** Everything written in the group since that person last called that agent, oldest first, each message with who wrote it, and a line that says who it works for and that only that person's words are to be followed. The others' messages are what they said, never instructions. Its own program keeps what came before.
- **What the group gets.** When the turn ends, the agent's last words are posted in the group as a message of its own kind: "Ada's Claude Code · MBP-Lorenzo", with a link to its chat. While it works, the group shows that it does. A turn that failed posts why.
- **The rest is in its chat:** the commands it ran, what it changed line by line, "Commit and open pull request", and a host it was refused, each for the chat's owner as today. Allowing a host starts the agent again, and that answer goes to the group too.
- **Such a chat is written to from its group only,** so the group and the chat can't tell two stories.
- **It is read by the group's members only,** not by the whole team as other chats are, since it holds what was written in the group.

## When a tag can't be followed

The message is posted either way, and a line under it says why nothing runs:

- the group has no project, or its project has no mapped branch;
- the person has no machine with that agent signed in;
- that agent is still working on their last tag in this group.

## What the web app shows

- **In a group's page:** a tag in a message is set apart; an agent's answer shows whose agent it is and on which machine, with "Open its chat"; "Ada's Claude Code is working…" while a turn runs. Under the box, the agents the person can call there, or what is missing.
- **In the chat's page:** which group it belongs to, with a link, and no box to write in.
- **The sidebar** lists such a chat under "Your chats", named after its group.

## Where the code goes

| Where | What |
| --- | --- |
| A migration | `group_id` on `chats`; on `group_messages`, the chat an answer came from |
| `apps/server/src/groups.rs` | Finding the tags in a message, and starting the chat or its next turn |
| `apps/server/src/chats.rs`, `db.rs` | A turn's end posted to the chat's group; who may read and write such a chat |
| `apps/web/src/routes/Group.tsx`, `Chat.tsx` | The answer in the group, and the group on the chat |
| `docs/architecture.md` | Agents in groups |

Nothing changes in the core or in `agent-run`: to a machine it is a chat's turn like any other.

## Not in this plan

- **Asking for somebody else's agent,** with their approval.
- **`@weacon`, the team's own agent** on the team's API key, which answers from the docs and the tasks without anybody's machine. It needs an agent that runs on the server, and is a plan of its own.
- An agent that speaks without being tagged, and one agent calling another.
- Choosing the branch or the machine for a group's agent.

## Tests

In `apps/server/tests/api.rs`, with a stand-in for the machine:

- A tag in a group with a project starts a chat of who wrote it, on their machine, and the machine is sent a turn with the group's messages since their last call, each with its writer.
- A second tag by the same person is that chat's next turn, with only what was written since; another person's tag starts a chat of their own.
- A turn's end posts the agent's last words in the group, from that chat; a failed turn posts why.
- A tag is not followed, and says why, without a project, without a machine that has the agent, and while the agent still works.
- A group's chat is read by the group's members and by no other member of the team, and its owner can't write in it from its own page.

That the real agents answer in a group is checked by hand.

## Steps

- [x] Server: tags, the chat of a group, and what the agent is told, with their tests
- [x] Server: a turn's end posted to the group, and who reads such a chat, with their tests
- [x] Web app: the answer and the working line in a group, and what can be called there
- [x] Web app: the group on a chat's page
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand with both agents: a tag in a group answers there, a second one goes on from the first, and a task made by the agent says where it came from

## Outcome

Built as planned, with these differences:

- **Why a tag isn't followed is told to who wrote it,** in the answer to their message, and shown above the box until they write again. It isn't kept in the group.
- **Such a chat goes with its group** when the last member leaves, so that it never becomes readable by the whole team.
- **An agent called for the first time in a long group is told its last fifty messages.**
- **The chat is called after its group,** `In the group "Flaky tests"`, which is the first line of what the agent is told.
- **What can be called there** is one line under the box; a missing machine or sign-in is said when a tag is tried.

- **Tests:** 62 on the server, one of them new, which covers the plan's list.
- **Checked by hand through Weacon** in the dev team's group of Ada and Marco, about ContentWrite, with the real programs on this machine. Ada's `@claude` answered in the group from the repository, knowing what Marco had written; a second `@claude` went on from the first and summed up both its answer and Marco's reply; `@codex` made the task Marco had asked for, CON-13, and said so in the group. The group showed whose agent was working, each answer named the agent and the machine with a link to its chat, and the chat's page names its group and has no box.
- **Not checked by hand:** a second person calling an agent of their own, since only this machine runs a core; a tag that can't be followed, in the browser. The test covers both.
