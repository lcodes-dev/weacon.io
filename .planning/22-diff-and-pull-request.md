# Plan 22: a chat's diff and its pull request

**Status: done** (2026-11-23)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: what an agent changed in a chat can be read line by line in the web app, and one button puts it on a branch and opens a draft pull request. Until then a chat's work stays in a folder on one machine and is of no use to anyone.

## What the person sees

- **The diff.** Under a turn's list of changed files, each file opens to its changed lines, added in green and removed in red. It shows everything the chat changed since it started, not one turn's share. Teammates see it too, also when the machine is away.
- **"Commit and open pull request".** Shown to the chat's owner when files are changed and no turn runs, with one field for the commit's message, filled in with the chat's title. Pressing it commits everything, pushes a new branch and opens a draft pull request against the branch the chat started from.
- **Afterwards** the chat shows the pull request's number and link, to everyone who can read it. When later turns change more, the button reads "Commit and push" and adds a commit to the same branch.

## How it works

1. **The diff comes with the end of a turn.** The core works it out with git and sends it in the event that ends the turn. The server keeps the newest one for each chat, beside the chat and not among its events, so that a chat's page doesn't load every old diff. Above 400,000 characters it is left out, and the page says so and shows the files' names and counts as today.
2. **The server asks the machine to push.** It makes a GitHub token for that one repository that may write its contents, good for an hour, and sends it with the branch's name, the message and the person's name and mail. The request waits for the answer, as a docs request does.
3. **The core commits and pushes.** It commits when something changed, and pushes in either case, so pressing again after a lost connection is safe. It answers with the commit, and keeps the token nowhere.
4. **The server opens the pull request** when the chat has none, as a draft, with the message as its title and a body saying who asked and linking to the chat. Its number and address are saved on the chat.

The branch is named `weacon/` plus the chat's title in lower case with dashes, plus four characters of the chat's id, and is picked at the first push.

## The team's GitHub App needs more

A team's app was registered to read contents only. Pushing needs it to write contents, and a pull request needs that permission too.

- **A new app asks for both** when it is registered.
- **An app that exists can't be changed by Weacon.** Its owner widens it on GitHub, in the app's settings, and then accepts the change where it is installed. The server sees what the installation allows; until it allows both, the chat shows a line saying what the team's owner has to do, with the link, in place of the button.
- **Every other token stays read-only.** The code map's and a turn's tokens are asked for with reading alone, so that widening the app widens nothing else.

## Git's own folder moves out of the agent's reach

Today the core runs git in a folder the agent may write anywhere in, `.git` included. An agent misled by a repository's text could write a hook or a setting there, and the core's next git command would run it outside the sandbox. From this plan on that command carries a token that can write to the repository.

So the core keeps git's folder beside the copy, in `chats/{id}/git`, where the sandbox lets nothing write, and names it on every git command. The copy keeps a `.git` file pointing there, so the agent can still read `git status` and `git diff`. The core never reads that file.

A chat made before this plan keeps its old folder and can't be pushed; there are two, both made while checking plan 21.

## Where the code goes

| Where | What |
| --- | --- |
| `crates/chat-copy/` (new) | A chat's copy of its repository: fetching it, what changed in it with the diff, and committing and pushing. Moved out of the core because it now has rules of its own, and because here it can be tested against a repository in a folder |
| `crates/core-link/` | The diff in the event that ends a turn; a `push` message, answered like an ask |
| `apps/core/` | Calls the crate, and answers a push |
| `apps/server/` | `chats.rs`: the diff kept on the chat, the pull request's route. `github.rs`: what the installation allows, a writing token for one repository, opening a pull request. One migration adds the diff, the branch pushed to and the pull request to `chats` |
| `apps/web/` | The diff under a turn's files, the button with its field, the pull request's link, the line for an app that allows too little; the new permissions in the app's registration |

## Not in this plan

Notes on a line of the diff; merging, or showing a pull request's checks and reviews; bringing in what happened on the base branch since, and conflicts; an agent writing the commit's message; the pull request on a task; deleting a chat, its folder or its branch. The downloadable core is still a plan of its own.

## Tests

- **`crates/chat-copy/tests/`**, against a repository made in a temporary folder: a copy is fetched; a changed, a new and a removed file come back with their lines and the diff; a commit is pushed to a new branch and a second one after it; a hook and a setting written into the copy's `.git` are not run.
- **`apps/server/tests/api.rs`**, with the made-up core and GitHub: a turn's diff is read by a teammate, and the newest replaces the last; the button asks GitHub for a writing token for one repository, the core gets the push, and a draft pull request is opened and shown; pressing again pushes and opens no second one; a teammate can't press it; an app that only reads gives the line and not the button; a machine that is away says so.

## Steps

- [x] `crates/chat-copy`: fetch, changes with the diff, commit and push, with git's folder out of the copy
- [x] The core uses the crate, sends the diff and answers a push
- [x] The server: the diff, what the app allows, the writing token, the pull request
- [x] The web app: the diff, the button, the link, the app's permissions
- [x] Docs: `docs/architecture.md` and `README.md`
- [x] Checked by hand on ContentWrite with both agents, after widening the dev team's app

## Outcome

Built as planned, on the owner's answers of 22 November, with these differences:

- **The commit is by the person as their own git on the machine knows them,** as the owner asked, and by their Weacon name and mail where it knows neither. Nothing else of the person's git settings is read, so a commit isn't signed.
- **The server doesn't look up what the app allows.** It asks GitHub for the writing token when the button is pressed, and GitHub's refusal becomes the line that says what the team's owner has to change. So the button is always there, and nothing is asked of GitHub while a chat is only read.
- **Two tokens, not one:** the machine's writes contents, the server's writes pull requests, each for the one repository.
- **The diff has an address of its own,** so that the page's request every second doesn't carry it.
- **Only the newest finished turn's files open to their lines,** since the diff is of the chat as it is now.
- **The button stays after a push,** as "Commit and push", also when nothing new was changed; the push is then the same commit again.
- **An address that isn't `https` is refused by the core,** and git is told to use no other protocol, since some kinds of address name a program to run.
- **Tests:** 53 in the server, one new; 3 in the new crate, one of which shows that a hook and a setting written into the copy run under plain git and not under the crate.
- **The token that opens the pull request also reads contents.** With pull requests alone GitHub refused to open one, which only the real GitHub showed. The server now logs what GitHub says when it refuses, which it had thrown away.
- **Checked by hand** on the dev server with a real core on ContentWrite, once the owner had widened the dev team's app. A Codex chat's two files opened to their changed lines. The button first gave the line about an app that only reads, then, with one permission still missing, pushed nothing. With both, it made the commit by the name and mail of the machine's own git, pushed `weacon/add-a-file-notes-md-with-one-line-saying-a947` and opened draft pull request 1, by the team's app, with the link to the chat. A second turn and "Commit and push" added a commit to the same pull request. Pressed while the core was dialling again, it said the machine was away. In a Claude Code chat the diff showed too, and the agent's attempt to write a hook into `.git` failed.
- **Not checked:** a push from a Claude Code chat, which runs the same code as Codex's; the two chats from plan 21, which show no changed files any more.
