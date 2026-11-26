# Plan 34: an agent proposes a change to the docs

**Status: done**

<!-- Status is one of: planned, in progress, done. Tick each step as it lands; the plan is done when every step is ticked. -->

Goal: an agent that finds a page of the docs wrong, or that changed the code a page describes, proposes the page's new text, and a person accepts it from the diff. The prototype shows it on a page: "Proposed by Claude Code in “Returns link on mobile”, from the code it changed". It needs plans 15, 28 and 33.

## The rule

Plan 15's rule holds: **anything an agent writes in the docs is a proposal, and changes nothing until a person accepts it.** Today only the docs' writer makes proposals, in a run or a request. This plan lets the agents make them too, with one more tool.

## The tool

`propose_doc`, beside the six tools an agent has, for a chat's agent and for `@weacon` alike.

| It takes | What it is |
| --- | --- |
| `title` | The page to change, by its title as the `doc` tool reads it. A title no page has makes a new page |
| `markdown` | The page's whole new text |
| `reason` | Why, in a sentence |
| `section` | For a new page only: "How it works" or "How it's built". Left out, the second |

- **The agent sends the whole page,** read first with the `doc` tool, since a diff is made from two whole texts. What it dropped by mistake shows in the diff as removed.
- **A changed page keeps the files it was written from.** A new page has none until the writer's first check, as a page a person made.
- **Refused:** a text the same as the page's, an empty one, one over a page's size limit, and a title that can't be a page's.
- **It can't remove a page.**

## What a person sees

- **On the docs page** the proposal is in the list with the writer's, and opens as the same diff with the same accepting by block. Over it, who proposed it: "Proposed by Ada's Claude Code in “Returns link on mobile”", with a link to the chat, or "Proposed by Weacon for Ada".
- **In the agent's chat** a line says what it proposed, with a link to the docs, as for a task it made.
- **In a group** the agent's own answer says so.

## What is kept

A migration adds to `proposals` who proposed: the agent, the person it works for, and the chat if there is one. A proposal of the docs' writer has none of the three.

- **A docs run drops only the writer's own unanswered proposals,** where today it drops all of them. An agent's proposal stays until a person answers it.
- **A deleted chat leaves its proposals,** without the link.
- **Accepting works as today:** the page's new version is the agent's, or the person's who took only some of its changes. A proposal written against an older version of its page shows the warning plan 15 has.

## Where the code goes

| Where | What |
| --- | --- |
| A migration | Who proposed, on `proposals` |
| `apps/server/src/tools.rs` | The tool, and what it refuses |
| `apps/server/src/db.rs` | Saving an agent's proposal; a run's delete narrowed to the writer's |
| `apps/web/src/components/DocProposal.tsx`, `ChatEvents.tsx` | Who proposed it; the line in the chat |
| `docs/architecture.md` | The tool, and proposals by agents |

Nothing changes in the core or in `agent-run`: the tool is listed with the others.

## Not in this plan

- An agent proposing without being asked at the end of every turn. The tool's description says when to use it; whether it does is the agent's.
- The writer's notes on a person's words, for an agent's proposal. The check after a person's edit still runs.
- Proposing to remove or rename a page.
- Telling the team about a new proposal outside the docs page.

## Tests

In `apps/server/tests/api.rs`:

- A chat's agent proposes a change to a page: the docs list it with who proposed it and the chat, the chat has its line, and accepting it makes the page's new version.
- A title no page has proposes a new page, in the section given.
- `@weacon` proposes for who tagged it, with no chat.
- A docs run afterwards drops the writer's proposals and keeps the agent's.
- The same text, an empty text and an empty title are refused, and say why to the agent.

That the real agents propose something worth accepting is checked by hand.

## Steps

- [x] Server: who proposed on `proposals`, and a run that drops only the writer's, with its test
- [x] Server: the tool, for a chat's agent and for `@weacon`, with their tests
- [x] Web app: who proposed it on the docs page, and the line in the chat
- [x] Docs: `docs/architecture.md`
- [x] Checked by hand on ContentWrite: a chat's agent changes code and proposes the page's change; `@weacon` proposes a correction in a group; both accepted from the diff

## Outcome so far

Built as planned, with these differences:

- **The link to the chat is left out for a group's chat,** which only the group's members read.
- **The title line the `doc` tool shows over a page is taken off a proposed text.** The first real run sent it back as the page's first heading.
- **No size limit of its own:** a page has none today either, beyond what a request may carry.
- **Questions 72 and 73 were built with their defaults:** `@weacon` proposes too, and a run keeps the agents' proposals.

- **Tests:** 64 on the server, one of them new, for a chat's agent; the test of `@weacon` has it propose a page too.
- **Checked by hand** on ContentWrite with the real Claude Code on this machine: asked to add a section to a page, it read the page and the code, and proposed the change; the chat said so with a link to the docs, and the docs page showed the diff under "Proposed by Ada Dorn's Claude Code · Open its chat". That proposal was then rejected.
- **Not checked by hand:** `@weacon` proposing in a group, which spends the team's real key, and accepting an agent's proposal in the browser. The tests cover both.
