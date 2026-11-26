# Plan 35: a WebSocket to browsers

**Status: in progress**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a page shows what changed when it changes, without asking again every second or two. Today ten places in the web app ask on a timer:

| Where | How often |
| --- | --- |
| A chat's page | Every second while its agent works, else every 5 s |
| A group's page | Every 2 s |
| The docs page | Every 2 s while a run or a check is on |
| A project's branches | Every 3 s while one waits for its map, else every minute |
| The sidebar's groups, chats and machines; the tasks; the machines page | Every 5 s |
| Whether the server answers | Every 5 s |

## The rule

**The socket says what changed, never the change itself.** A page still reads everything with the requests it has today; the socket only tells it when to ask. So who may read what is decided where it is today, in each request, and the socket needs no rules of its own beyond whom it tells.

## How it works

1. A signed-in browser opens one WebSocket, `/api/live`, with its session's cookie, as the sign-in terminal's socket does.
2. Where the server changes something others watch, it says so in one line: what kind of thing, and whom to tell. The kinds are the web app's own names for what it reads: a chat, the chats, a group, the groups, the tasks, the docs, the projects, the machines.
3. The socket of each person told gets the kind, and the web app asks again for what it shows of that kind. A page that shows nothing of it does nothing.

- **Whom to tell** is a team's members, or named people. A group's news goes to its members only, since nobody else may know the group is there; a machine's goes to its owner; the rest to the team.
- **No id is sent.** "A chat changed" has every open chat's page ask for what came after its last event, which is the cheap request it makes today.
- **When the socket closes,** the web app opens it again after a moment, and asks again for everything it shows, since it may have missed something. The dot in the sidebar that says whether the server answers becomes whether the socket is open.
- **The timers go,** all ten.

In the server it is one channel that every socket listens to, kept in memory. A restart closes the sockets, and the browsers come back by themselves.

## Where the server says so

| What happened | Kind, and whom |
| --- | --- |
| A message in a group, an agent's answer there, a member joining or leaving, a name or project given | The group and the groups, to its members |
| A chat made, written in, deleted; an event of its turn from the machine; a note the server adds | The chat and the chats, to the team |
| A task made, changed, deleted or moved, by a person or an agent | The tasks, to the team |
| A docs run's progress and end, a check's end, a page or a proposal changed | The docs, to the team |
| A branch mapped, a project made, changed or deleted | The projects, to the team |
| A core connecting or going away, an agent signed in, a machine paired or removed | The machines, to its owner |

## Where the code goes

| Where | What |
| --- | --- |
| `apps/server/src/live.rs` | The channel, the one function that says what changed, and the socket |
| The modules named above | A call to that function where each thing changes |
| `apps/web/src/live.ts` | The socket, opened once in the shell: a kind in, the matching requests asked again |
| The pages | Their timers removed |
| `docs/architecture.md` | The socket |

No migration and no crate. No new dependency: the server already speaks WebSocket to the cores.

## Not in this plan

- Sending the data itself, such as an agent's words as they are written.
- Who is writing in a group, and who has a page open.
- Anything for the cores, whose socket is its own.
- More than one server sharing the channel.

## Tests

In `apps/server/tests/api.rs`, with the WebSocket client the tests have for a machine:

- A member with the socket open hears "a group" when another member writes in their group; a member of the team who isn't in the group hears nothing.
- A machine's event in a chat is heard by the team as "a chat"; a member of another team hears nothing.
- A task changed by an agent's tool is heard as "the tasks".
- A socket without a session is refused.

That a second browser shows a message at once, and that a page catches up after the server restarts, is checked by hand.

## Steps

- [x] Server: the channel and the socket, with groups and chats saying what changed, and their tests
- [x] Web app: the socket, and the chat's, the group's and the sidebar's timers removed
- [x] Server and web app: tasks, docs, projects and machines, and the remaining timers removed
- [x] Docs: `docs/architecture.md`
- [ ] Checked by hand in two browsers: a group's message and an agent's turn show at once; a restart of the server is caught up with

## Outcome so far

Built as planned, with these differences:

- **Six kinds, not eight.** A chat and the chats were always told together, and so were a group and the groups, so each pair is one word: `chats`, `groups`, `tasks`, `docs`, `projects`, `machines`.
- **Whom to tell is always a list of people,** read when the change is told: a team's members, a group's, or a machine's owner. So a socket knows only its person, and one rule covers every case.
- **A chat called in a group is told to the group's members only,** as only they may read it.
- **A machine that comes or goes is told to its owner as `machines` and `chats`,** since their chats on it show it too.
- **Built in one step,** not two: no page was left on a timer in between.
- **Question 74 was built with its default:** all ten timers are gone.

- **Tests:** 65 on the server, one of them new: who hears what of a group, a chat, a machine's event, an agent's task and a machine going away, and who is refused the socket.
- **Checked by hand** in one browser: the dot is green with the socket open; a message posted to a group from outside the page showed on the open page within a second; the machine in the sidebar went to connected by itself; an idle page made no request in twenty seconds; a page loaded while the server was still starting opened its socket once it was up.
- **Not checked by hand:** two browsers side by side, and an agent's turn showing as it runs.
