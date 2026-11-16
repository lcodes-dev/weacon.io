import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "../incoming.ts";
import {
  type Ask,
  type Worktree,
  UNASKED,
  answered,
  busy,
  cancelled,
  ended,
  envelopeFor,
  removeQuestion,
  resumeLine,
  stateLine,
  unsent,
  waiting,
} from "./worktree.ts";

const REF = { workspace: "w-1", chat: "c-1" };
const ON = { ...REF, machine: "mc-1" };

type Heard = Extract<Incoming, { type: "worktree_answer" | "error" }>;

const state = (request: string, removed: boolean, uncommitted = true, unpushed = true, chat = "c-1"): Heard => ({
  id: 0,
  type: "worktree_answer",
  workspace: "w-1",
  chat,
  request,
  answer: { kind: "state", removed, uncommitted, unpushed },
});
const refused = (id: number, code: string): Heard => ({ id, type: "error", code, retryable: false });

// `worktree` waiting for `ask`, sent as message `id` under request `q-<id>`.
const asked = (worktree: Worktree, id: number, ask: Ask): Worktree =>
  waiting(worktree, { id, request: `q-${id}`, ask: ask.kind, confirm: ask.kind === "state" && ask.confirm === true });
const hear = (worktree: Worktree, message: Heard) => answered(worktree, REF, message);

test("each ask goes out as the protocol's query or command, under its id and request", () => {
  assert.deepEqual(envelopeFor(ON, 3, "q-1", { kind: "state", confirm: true }), { id: 3, type: "worktree_query", ...ON, request: "q-1", query: { kind: "state" } });
  assert.deepEqual(envelopeFor(ON, 4, "q-2", { kind: "open_pull_request", title: "Sort the pantry", body: "" }), {
    id: 4,
    type: "worktree_command",
    ...ON,
    request: "q-2",
    action: { kind: "open_pull_request", title: "Sort the pantry", body: "" },
  });
  assert.deepEqual(envelopeFor(ON, 5, "q-3", { kind: "remove" }), { id: 5, type: "worktree_command", ...ON, request: "q-3", action: { kind: "remove" } });
});

test("only an action, or the state Remove asks for, keeps the bar busy", () => {
  assert.equal(busy(asked(UNASKED, 3, { kind: "state" })), false);
  assert.equal(busy(asked(UNASKED, 3, { kind: "state", confirm: true })), true);
  assert.equal(busy(asked(UNASKED, 3, { kind: "remove" })), true);
});

test("an older answer can't replace a newer one, and answers to nothing asked, or about another chat, change nothing", () => {
  const out = asked(asked(UNASKED, 3, { kind: "state" }), 4, { kind: "remove" });
  const removed = hear(out, state("q-4", true));
  const late = hear(removed, state("q-3", false));

  assert.deepEqual(late.standing, { kind: "state", removed: true, uncommitted: true, unpushed: true });
  assert.deepEqual(late.pending, []);
  assert.equal(hear(late, state("q-9", false)), late);
  assert.equal(hear(late, refused(9, "request.unknown")), late);
  const other = asked(UNASKED, 5, { kind: "state" });
  assert.equal(hear(other, state("q-5", false, true, true, "c-2")), other);
});

test("an older `worktree.none` can't replace a newer state, or end the Remove it opened", () => {
  const out = asked(asked(UNASKED, 3, { kind: "state" }), 4, { kind: "state", confirm: true });
  const confirming = hear(out, state("q-4", false));
  const late = hear(confirming, refused(3, "worktree.none"));

  assert.equal(late.standing.kind, "state");
  assert.notEqual(late.confirming, undefined);
});

test("Remove confirms against the fresh state it asks for, and any newer state ends it", () => {
  const known = hear(asked(UNASKED, 3, { kind: "state" }), state("q-3", false, false, false));
  const confirming = hear(asked(known, 4, { kind: "state", confirm: true }), state("q-4", false, true, false));

  assert.deepEqual(confirming.confirming, { kind: "state", removed: false, uncommitted: true, unpushed: false });
  assert.equal(cancelled(confirming).confirming, undefined);
  assert.equal(hear(asked(confirming, 5, { kind: "state" }), state("q-5", false)).confirming, undefined);
  // A worktree already removed has nothing to confirm.
  assert.equal(hear(asked(known, 6, { kind: "state", confirm: true }), state("q-6", true)).confirming, undefined);
});

test("the end of the connection drops what was asked and what was being confirmed, and keeps what is known", () => {
  const confirming = hear(asked(UNASKED, 3, { kind: "state", confirm: true }), state("q-3", false));
  const out = asked(confirming, 4, { kind: "remove" });

  assert.deepEqual(ended(out).pending, []);
  assert.equal(ended(out).confirming, undefined);
  assert.deepEqual(ended(out).standing, confirming.standing);
});

test("a chat with no worktree says so, and an action refused or unsent says what didn't happen", () => {
  const none = hear(asked(UNASKED, 3, { kind: "state" }), refused(3, "worktree.none"));

  assert.deepEqual(none.standing, { kind: "none" });
  assert.equal(stateLine(none.standing), "This chat has no worktree yet.");
  assert.deepEqual(hear(asked(none, 4, { kind: "open_terminal" }), refused(4, "request.unknown")).outcome, {
    kind: "problem",
    text: "The terminal didn't open. On a machine without a desktop terminal, run the line below there.",
  });
  assert.deepEqual(hear(asked(none, 4, { kind: "remove" }), refused(4, "worktree.busy")).outcome, {
    kind: "problem",
    text: "Stop the agent first, then remove the worktree.",
  });
  assert.deepEqual(unsent(none, "remove").outcome, { kind: "problem", text: "The worktree wasn't removed. The connection is down." });
  assert.equal(waiting(unsent(none, "remove"), { id: 5, request: "q-5", ask: "remove", confirm: false }).outcome, undefined);
});

test("a pull request or a terminal says what came of it, with the page to open", () => {
  const answer = (id: number, kind: string, url?: string): Heard => ({ id: 0, type: "worktree_answer", ...ON, request: `q-${id}`, answer: { kind, url } as never });
  const pr = asked(UNASKED, 3, { kind: "open_pull_request", title: "t", body: "" });
  const terminal = asked(UNASKED, 4, { kind: "open_terminal" });

  assert.deepEqual(hear(pr, answer(3, "pull_request_opened", "https://example.com/pr/1")).outcome, {
    kind: "link",
    text: "A draft pull request is open.",
    url: "https://example.com/pr/1",
  });
  assert.deepEqual(hear(pr, answer(3, "pull_request_page", "https://example.com/compare")).outcome, {
    kind: "link",
    text: "The branch is pushed. Open the pull request from its page.",
    url: "https://example.com/compare",
  });
  assert.deepEqual(hear(terminal, answer(4, "terminal_opened")).outcome, { kind: "note", text: "A terminal opened on the chat's machine." });
});

test("the state line and Remove's question say what is there, what goes and what stays", () => {
  const at = (removed: boolean, uncommitted: boolean, unpushed: boolean) => ({ kind: "state" as const, removed, uncommitted, unpushed });

  assert.equal(stateLine({ kind: "unknown" }), "Checking the worktree.");
  assert.equal(stateLine(at(true, true, true)), "The worktree was removed. The chat's next message checks it out again.");
  assert.equal(stateLine(at(false, true, true)), "Uncommitted changes · Commits not pushed");
  assert.equal(stateLine(at(false, false, true)), "Commits not pushed");
  assert.equal(stateLine(at(false, false, false)), "Everything is committed and pushed.");
  assert.equal(
    removeQuestion(at(false, true, true)),
    "Remove this chat's worktree? Its uncommitted changes are deleted for good. Its branch stays, with commits that aren't pushed yet. The chat's next message checks it out again.",
  );
  assert.equal(removeQuestion(at(false, false, false)), "Remove this chat's worktree? Its branch stays. The chat's next message checks it out again.");
});

test("asking the state again keeps the last action's outcome, and the next action clears it", () => {
  const pr = asked(UNASKED, 3, { kind: "open_pull_request", title: "t", body: "" });
  const linked = hear(pr, { id: 0, type: "worktree_answer", ...ON, request: "q-3", answer: { kind: "pull_request_page", url: "https://example.com/compare" } });

  assert.deepEqual(asked(linked, 4, { kind: "state" }).outcome, linked.outcome);
  assert.equal(asked(linked, 5, { kind: "state", confirm: true }).outcome, undefined);
  assert.equal(asked(linked, 6, { kind: "remove" }).outcome, undefined);
});

test("the resume line names the chat and its workspace", () => {
  assert.equal(resumeLine({ workspace: "w-1", chat: "c-1" }), "weacon resume --workspace=w-1 --chat=c-1");
});
