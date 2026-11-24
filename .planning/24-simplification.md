# Plan 24: simplifying chats

**Status: done** (2026-11-24)

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: the code of plans 21 to 23 does the same with less. The owner asked on 24 November for a review of all of it with simplicity above everything else, and for a proposal before any change. Nothing here adds a feature; one proposal takes a little away.

What was read: `crates/agent-run`, `crates/chat-copy`, `crates/core-link`, the core's `chats.rs` and `link.rs`, the server's `chats.rs`, its chat queries and its additions to `machines.rs` and `github.rs`, and the web app's chat pages. About 2,400 lines without tests.

## 1. One way to allow a host, not two

This is where most of the complexity is, and it came in with plan 23.

Today the two agents learn of a host in different ways. Codex is refused, the chat shows the host, and a next turn tries again. Claude Code asks while its command waits, which needs: its input kept open and written to in its own protocol; a list of what it asked; a refusal of everything else it might ask; an id on the event; a message to the core and a channel into the running turn; two columns on the chat for what it waits for, set and cleared in four places; a second kind of button, "Deny", and the sidebar's mark.

Tried on 24 November: started as before plan 23, with nobody to ask, Claude Code refuses the host and names it in the command's result, as Codex does (`deny network-outbound github.com:443`). So Claude Code can work exactly like Codex.

**Proposed:** both agents are refused, the chat shows the host with "Allow and try again", and the next turn has it. All of the above goes.

**What is given up:** Claude Code no longer carries on in the middle of a command after a yes; it needs the next turn, as Codex does. There is no "Deny", since a refused host is already refused, and no mark in the sidebar, since no chat waits.

**What it removes,** roughly 300 lines and most of what is hard to follow: the answer message and channel; `asking` and `asking_host`, by a new migration; the id on `wants`; the route's second branch; half of `agent-run`'s loop; the stand-in's asking; one test in each of the two suites, and half of another.

## 2. `agent-run`: one file for each agent

`lib.rs` holds both vendors' flags and both vendors' output, 400 lines. After 1 the part that runs a program is about 60 lines.

**Proposed:** `claude.rs` and `codex.rs`, each with the two things that are that vendor's business: how it is started, and what a line it prints means. `lib.rs` keeps the turn itself, the cache settings and the words they share.

## 3. The core: a chat's file is its only state

`chats::turn` does three things in one function of 90 lines: it tells again what the server missed, it finds a turn that a restart lost, and it starts a turn. Beside the file of events the core also counts them in memory, and the two are brought back in step at the top of that function.

**Proposed:** two functions, one that catches the server up and one that runs a turn. An event's number is the line it is written on, so the count in memory goes, and what the core remembers of a chat is only how to stop its running turn.

## 4. The server: say each thing once

- **The checks before a chat's machine is used** are written out in `say`, `stop`, `push` and `answer`: the machine was removed, the machine is away, the agent is still working. One function gives the chat's connected machine or the sentence that says why not.
- **Two pairs of queries differ by one condition:** a team's chats and one chat, with sixteen columns each; the running turns of a machine and one running turn. Each pair becomes one query that takes the id or nothing. Both stay on the indexes they use now.
- **Three things are called `Turn`:** what the server reads from its database, what it sends, and what `agent-run` takes. The database's becomes the one that is sent, built in the query's function.

## 5. `chat-copy`: no chats from before it

It still knows the folder layout of chats made before plan 22, to leave them alone, and has an error for pushing one. The two there were are gone with their machine.

**Proposed:** both go.

## 6. The web app: the chat page in pieces

`Chat.tsx` is one component of 300 lines with five requests, and the list of events has buttons folded into it by a condition six lines long.

**Proposed:** the page keeps the chat's request and its layout. The header with the hosts, the list of events, the form that pushes and the box to write in are a component each, in the same file's folder, each with the one request it makes. After 1 a wanted host is a line and one button.

## Looked at and left

- **Two GitHub tokens for a push.** One would be less code, and would put the right to open pull requests on the machine.
- **The diff kept on the chat and not in its events.** Folding it back in would have every request for news carry it.
- **A push answered like an ask, not told as events.** It reuses what docs requests already have.
- **Events numbered and taken only in order.** It is what lets a turn outlive the connection, and it is small.
- **A chat's hosts as a JSON list on its row.** A table of their own would be more usual and would add a query to every reading of a chat.
- **The list of package tools and their settings.** It is the special cases the owner chose over allowing folders by name.

## Tests

No new behaviour, so the tests change only where 1 takes something away: the server's test of hosts loses asking, answering under an id and "Deny"; `agent-run`'s loses its test of answers and gains Claude Code's refusal read from a recorded result. Everything else must pass as it is, which is the check that nothing else changed.

## Steps

- [x] One way to allow a host: `agent-run`, `core-link`, the core, the server with its migration, the web app
- [x] `agent-run` in one file for each agent
- [x] The core's chat code in two functions, with the file as its only state
- [x] The server's checks, queries and the name `Turn`
- [x] `chat-copy` without the old layout
- [x] The chat page in pieces
- [x] Docs: `docs/architecture.md`, and plan 23's outcome
- [x] Checked by hand with both agents: a host allowed and a package installed

## Outcome

Done as proposed, with these differences:

- **About 150 lines fewer in the code and 100 fewer in the tests,** not the 300 guessed for the first proposal alone. What went is the hard part, though: nothing waits to be answered any more, on the machine, on the server or in the page.
- **The database's `Turn` is renamed `Running`,** not turned into the one that is sent: the server still needs the team and the repository beside it, to ask GitHub for the token.
- **One of the two pairs of queries became one query; the other lost a member.** A team's chats and one chat are one query. The one running turn is found among its machine's running turns, so its own query is gone.
- **A bug found on the way, and fixed.** A request that saved a new turn and was then dropped by the browser left the turn saved and the machine never told, until the core next connected. Seen in the hand check by leaving the page right after pressing the button; the gap was there since plan 21. The machine is now told on the side, whatever becomes of the request.
- **A race closed in the core.** An event told while the core was catching the server up could be sent ahead of older ones, and then be dropped by the server until the next reconnect. Writing and sending now happen one at a time.
- **Tests:** 54 in the server, the one of hosts rewritten and every other untouched; 5 in `agent-run`, where the test of answers went and both agents' refusals are read in one.
- **Checked by hand** with a real core on ContentWrite: in a Claude Code chat and a Codex chat, `npm install` was refused its registry, the chat showed the host, and after "Allow and try again" the next turn installed the package. The Claude Code turn that the dropped request had lost started when the core next connected.
- **Not checked by hand:** "Commit and push" and "Stop" in the page as it is now split, which the build and the server's tests cover.
