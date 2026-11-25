# Plan 31: groups, where a team's people talk

**Status: done** (2026-11-25)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the people of a team write to each other in Weacon, two of them or several. It is the last of the three things Weacon's own description names, and the place where plan 32 lets a person call their agent in front of the others. Today the only conversation is one person's with one agent.

This plan is people only. Plan 32 is agents in a group. The owner agreed on 25 November to what was asked: a direct chat is a group of two, a group is private to its members and has at most one project, given once, and asking again every two seconds will do for now.

## One thing: a group

A group is some members of one team and what they wrote to each other.

- **A direct chat is a group of two.** The prototype lists "Direct" and "Groups" apart; here they are one thing, kept and shown the same way. A group has a name or none: one without is called by its other members' names.
- **"Chat" keeps its meaning:** a person's conversation with an agent. A group is never called a chat.
- **Private to its members.** Only they read it and write in it, and only they see that it exists. A chat with an agent stays readable by the whole team, as today.
- **About a project, or about none.** A group can be given one project of its team, when it is made or once later while it has none. It is shown beside the group's name, and it is what plan 32's agents work on.

## What its members do

| | |
| --- | --- |
| Make one | Any member of the team, from the members they pick, with a name or without, and a project or none |
| Write | A message is Markdown text. It is not changed or deleted afterwards |
| Add someone | Any member of the group adds a member of the team, who then reads everything written before |
| Rename it, give it its project | Any member of the group |
| Leave | Each for themselves. A group nobody is in any more is deleted with its messages |

Someone removed from the team leaves its groups with it. What they wrote stays, under their name.

## What a member sees

- **The sidebar** lists the person's groups under "Groups", the one written in last first, each with a dot when it has messages they haven't read. "New group" is under them.
- **A group's page,** at `/groups/{id}`: its name, its members and its project at the top; the messages, oldest first, each with who wrote it and when; a box to write in at the bottom, sent with Enter. Opening the page marks the group read.
- **What is new arrives by asking again,** every two seconds while a group's page is open and every five for the sidebar, as a chat's page does today. A message written by somebody else so shows within two seconds. A WebSocket to browsers isn't built, and this plan doesn't build it.

## Where the code goes

| Where | What |
| --- | --- |
| `apps/server/migrations/0017_groups.sql` | `groups`, with its team, name and project; `group_members`, with the last message each has read; `group_messages`. An index for each list: a person's groups, a group's members, a group's messages |
| `apps/server/src/db.rs` | A person's groups with their members and whether each is unread; a group's messages after one; making, renaming, adding, leaving, writing, marking read |
| `apps/server/src/groups.rs` | The requests below |
| `apps/server/src/teams.rs` | A member removed from a team leaves its groups |
| `apps/web/src/routes/Group.tsx`, `NewGroup.tsx` | The page and the form |
| `apps/web/src/components/Shell.tsx` | "Groups" in the sidebar |
| `docs/architecture.md` | A section on groups |

| Request | What it does |
| --- | --- |
| `GET /teams/{id}/groups` | The person's groups in the team: name, members, project, and whether unread |
| `POST` the same | Makes one from `members`, and optionally `name` and `project_id` |
| `GET …/groups/{group_id}?after=` | The group and its messages, or those after one, and marks it read up to the last |
| `PUT …/groups/{group_id}` | Renames it, or gives it its project while it has none |
| `POST …/groups/{group_id}/members` | Adds a member of the team |
| `DELETE …/groups/{group_id}/members/me` | Leaves it |
| `POST …/groups/{group_id}/messages` | Writes a message |

No crate: a group has no rules beyond what one request checks.

## Where this differs from the project's spec and the prototype

- **No roles in a group.** The prototype shows "support" and "lead" beside a name; a team here has an owner and members, and a group has members.
- **No "is typing", reactions, threads, files or pictures.**
- **Nothing reaches a person outside Weacon:** no mail and no notification for a message.
- **No bridge to another chat program,** which the spec plans for later.

## Not in this plan

- Agents in a group: plan 32.
- Changing or deleting a message, and searching what was written.
- A WebSocket to browsers.
- Removing somebody else from a group.

## Tests

In `apps/server/tests/api.rs`, over HTTP:

- A member makes a group of two without a name and one of three with a name and a project; a person who isn't in the team, and a project of another team, are refused.
- A message is written and read by the others, in order; `after` gives only the later ones; an empty message is refused.
- A group is unread for the others after a message and read once its page was asked for; it never is for who wrote.
- Someone who isn't in a group can neither read it, write in it nor see it listed, though they are in the team.
- A member is added and reads what came before; a member leaves and reads nothing more; the last one leaving deletes the group.
- A group is renamed, and given a project once and not twice.
- A member removed from the team is gone from its groups, and their messages stay.

That the pages work is checked by hand in the browser, with two people signed in.

## Steps

- [x] Server: the migration, the queries and the requests, with their tests
- [x] Server: a member removed from a team leaves its groups, with its test
- [x] Web app: "Groups" in the sidebar and "New group"
- [x] Web app: a group's page
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand with two people in two browsers: make a group, write both ways, see the unread dot come and go, add a third, leave

## Outcome

Built as planned, with these differences:

- **A group's name, its project, adding someone and leaving are under "Group settings"** on its page, closed until opened.
- **The sidebar's two empty headings, "Direct" and "Groups", became the one list.**
- **The same two people can have several groups without a name.** Nothing looks for one that exists.

- **Tests:** 61 on the server, one of them new, which covers every line of the plan's list.
- **Checked by hand** in the dev app with two people, Ada in the browser and a test member, Marco, through the API: a group of the two about ContentWrite was made in the browser; each wrote and the other read, Markdown and all; Marco's reply showed on Ada's open page without a reload; a message written while Ada was on another page put the dot on the group in her sidebar, and opening the group took it off; the group was renamed under its settings.
- **Not checked by hand:** a second person in a browser of their own; adding a third member and leaving, which the test covers.
