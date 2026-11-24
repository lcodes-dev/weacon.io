# Plan 27: tasks

**Status: done** (2026-11-24; rewritten that day with the owner's answers)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: a team writes down what a project needs done, says who does it and how far it is, and starts a chat from it, on a list and a board as the prototype has them. Tasks are the one part of Weacon's own description with nothing built.

This plan is what people do with tasks. Two plans follow it: 28 lets a chat's agent read and write them, and 29 finds the docs and the code a task is about.

## What a task is

A task belongs to one project. It has:

| | |
| --- | --- |
| A name | The project's key and a number, as `SF-12`. The number is counted per project from 1, given by the server and never reused |
| A title | One line |
| A text | Markdown, optional |
| Checks | "Done when": a list of lines, each ticked or not |
| Docs it touches | Pages of the project's docs, picked from a list |
| A status | To do, Doing or Done |
| An owner | A member of the team, or nobody |
| Who asked | The member who made it, and when |
| A place | Where it stands among the tasks of its status |

Every member of the project's team may read a project's tasks, make one and change any of them. A task is deleted only by who asked for it or by the team's owner, as the owner chose on 24 November.

**A status is set by hand, and by nothing else,** as the owner chose on 24 November: three to start with, and no chat or pull request moves a task.

## A project's key

A project gets a key: two to five capital letters or digits, such as `SF`. The form for a project suggests one from its name and the person can type another; the server refuses one that another project of the team has. A key can be changed later, and every task of the project is then named by the new one, since a task keeps only its number.

**A changed key never loses a pull request,** which the owner made the condition on 24 November. A task is tied to its pull requests through its chats, in the database, and Weacon never looks for a task's name in a pull request's title or branch. Nothing Weacon writes to GitHub carries the name today: a chat's branch comes from its title, and so does its pull request. Should a later plan write the name there, it has to keep that tie in the database too, and an older pull request will then show the older name.

The projects that exist get the first three letters of their name, which the team can change.

## The order

Tasks are ordered by hand inside each status. A new task goes to the top of To do. Dragging a card on the board to another place, in its column or another, is one request: this task, into that status, before that task or at the end. The server then numbers that status again in one transaction. The list shows the same order, status after status.

## Tasks and chats

A chat can be for a task of its own project: one optional link on the chat, set when it starts and kept.

- **From a task:** "New chat for this task" opens the new chat page with the project fixed and the task's name, title, text and checks as the first message, for the person to change before sending.
- **On the chat:** its header names the task and links to it.
- **On the task:** its chats are listed, each with whose it is, its agent, whether a turn runs, and its pull request once it has one. The web app already holds the team's chats, so this needs no new query.
- **A deleted task** leaves its chats as they are, without the link.

## What the web app shows

The page at `/projects/{id}/tasks`, linked from the project's page beside its docs, as the prototype has it:

- **Two views,** "List" and "Board", chosen at the top and kept in the address. The list is a row per task with its name, title, status and owner. The board is a column per status with a card per task, dragged to move it; a card can also be moved with the keyboard, as a docs page can.
- **The panel beside either,** for the task picked: its name and status, title, who asked, the checks, each ticked in place, the docs it touches, its chats and their pull requests, and "New chat for this task". "Delete" shows to who may. Title, text, checks, docs, status and owner are each changed there.
- **"New task"** is the same panel, empty.

The sidebar's "Tasks" and "Docs" lines stay as they are, as the owner chose.

## Where the code goes

| Where | What |
| --- | --- |
| `apps/server/migrations/0014_tasks.sql` | `key` on `projects`; the `tasks` table, with its checks as JSON on the row; `task_docs`, a row per page a task touches; `task_id` on `chats`. An index for each: tasks by project and number, and by project, status and place; `task_docs` by page; chats by task |
| `apps/server/src/db.rs` | A project's tasks; making, changing, moving and deleting one; the task on a chat; the key on a project |
| `apps/server/src/tasks.rs` | The requests below |
| `apps/server/src/projects.rs` | A project is made and changed with its key |
| `apps/server/src/chats.rs` | A chat may start with a task of its project |
| `apps/web/src/routes/ProjectTasks.tsx` | The page, with its list, board and panel |
| `apps/web/src/routes/NewChat.tsx`, `Chat.tsx`, `Project.tsx` | Starting from a task, the task in a chat's header, the key in a project's form and the link to the page |
| `docs/architecture.md` | A section on tasks |

| Request | What it does |
| --- | --- |
| `GET /teams/{id}/projects/{project_id}/tasks` | The project's tasks, in order |
| `POST` the same | Makes one, from a title and optionally the rest |
| `PUT …/tasks/{task_id}` | Changes what it is sent of the title, text, checks, docs and owner, and leaves the rest |
| `POST …/tasks/{task_id}/move` | Puts it in a status, before another task or at the end |
| `DELETE …/tasks/{task_id}` | Deletes it |

No crate: a task has no rules of its own beyond what one request checks.

## Where this differs from the project's spec

- **No version on a task.** The spec gives each one, to tell two people's changes apart. Here a change carries only what it changes, so two people who change different things both keep theirs, and of two who change the same thing the later wins.
- **Whole numbers for the order,** where the spec has keys that let one row move. A status is numbered again on each move, which is one statement and a few hundred rows at most.

## Not in this plan

- Agents reading or writing tasks, and a task made from a chat: plan 28.
- Docs suggested for a task, and the code a task is about: plan 29.
- The prototype's "New" and "In review", and a status that follows a chat or a pull request.
- A task's name in a chat's branch or its pull request's title.
- Comments on a task, a history of its changes, and mail when a task is given to someone.

## Tests

In `apps/server/tests/api.rs`, over HTTP:

- A project is made with a key; a key another project of the team has, and one that isn't two to five letters or digits, are refused; a changed key renames the project's tasks.
- A member makes a task and it gets the next number of its project and the top of To do; a second project starts again from 1; a deleted task's number isn't given again.
- A task is changed: one thing at a time leaves the rest; an empty title, an owner who isn't in the team and a page of another project are refused. A check is added, ticked and removed.
- A task is moved inside its status and into another, before a task and to the end, and the lists come back in that order; a move before a task of another project is refused.
- A deleted page leaves the task without it.
- Someone outside the team can neither read nor change a project's tasks, and a task isn't found under another project's address.
- A task is deleted by who asked for it and by the team's owner, and another member is refused.
- A chat starts for a task and says so; a task of another project is refused; deleting the task leaves the chat without it.

That the page works, dragging included, is checked by hand in the browser.

## Steps

- [x] Server: a project's key, with the migration and its tests
- [x] Server: tasks, their checks, docs and order, with the requests and their tests
- [x] Server: a chat started for a task, with its tests
- [x] Web app: the key in a project's form
- [x] Web app: the tasks page, with its list and panel
- [x] Web app: the board, with dragging and the keyboard
- [x] Web app: a chat from a task, the task in a chat's header, the link on the project's page
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand in the browser: make, change, tick, move, give and delete a task; start a chat from one and find each from the other

## Outcome

Built as planned, with these differences:

- **No index on a project's tasks by number.** No request looks a task up by its number yet; plan 28's will.
- **A project counts the tasks it ever had,** in `tasks_made`, so that a deleted task's number isn't given again.
- **A key's uniqueness is checked by the request,** not by an index: a key is a name for people, and nothing is looked up by it.
- **A task's text is typed and shown as plain text** in the panel; its Markdown isn't rendered yet.

- **Simplified before the commit,** as the owner asked on 24 November:
  - A status is given its tasks as a list, in their order, in place of a move before another task: `PUT …/tasks` with `{"status", "tasks"}`, and no `…/move`.
  - A page that isn't the project's is left out of a task's pages, not refused.
  - A chat is given with its task's name, so its header asks for nothing more.
  - "New task" makes a task called "New task" and opens its panel; there is no form of its own for a new one.
  - The column for who asked is `asker_id`, as the API says.

- **Tests:** 57 on the server, three of them new: a project's key, a team's tasks, and a chat for a task.
- **Checked by hand** in the browser on ContentWrite, whose key became `CON`: a task was made, given a check, a page of the docs and an owner; a check was ticked; cards were dragged to another column and to another place, and one was moved with Space and the arrows; a chat started from CON-1 with Claude Code got its name, text and checks as the first message and wrote the file; the chat's header links to the task and the task lists the chat; a task was deleted.
- **Not checked by hand:** a second member, so that another person's task can't be deleted, which the tests cover; Codex from a task; a project made with a suggested key.
