import assert from "node:assert/strict";
import { test } from "node:test";

import type { DiffFile, Envelope } from "../protocol.ts";
import { NO_CHANGES, ask, canAsk, cut, needsAsking, receive, rows } from "./changes.ts";

const file = (path: string): DiffFile => ({
  path,
  change: { kind: "modified" },
  content: {
    kind: "text",
    hunks: [
      {
        old_start: 3,
        new_start: 5,
        lines: [
          { op: "context", text: "a" },
          { op: "removed", text: "b" },
          { op: "added", text: "c" },
          { op: "added", text: "d", no_newline: true },
        ],
      },
    ],
  },
});

const answer = (request: string, body: unknown, chat = "c-1", workspace = "w-1"): Envelope =>
  ({ id: 0, type: "diff_answer", request, workspace, chat, answer: body }) as Envelope;

test("asking sends a changes query and waits for its answers", () => {
  const { query, changes } = ask(5, "q-1", "w-1", "c-1", NO_CHANGES);

  assert.deepEqual(query, { id: 5, type: "diff_query", request: "q-1", workspace: "w-1", chat: "c-1", query: { kind: "changes" } });
  assert.deepEqual(changes, { asked: { id: 5, request: "q-1", workspace: "w-1", chat: "c-1" }, files: [], total: undefined, cut: false, held: false });
});

test("files arrive one answer each, and only the latest request's for its chat count", () => {
  const { changes } = ask(5, "q-2", "w-1", "c-1", NO_CHANGES);
  const of = (path: string, files = 2) => ({ kind: "file", files, file: file(path) });
  const one = receive(changes, answer("q-2", of("a.rs")));
  const two = receive(one, answer("q-2", of("b.rs", 3)));

  assert.deepEqual(one, { ...changes, files: [file("a.rs")], total: 2 });
  assert.deepEqual(two.files.map((each) => each.path), ["a.rs", "b.rs"]);
  assert.equal(two.total, 2);
  assert.equal(receive(two, answer("q-2", of("extra.rs"))), two);
  assert.equal(receive(one, answer("q-1", of("old.rs"))), one);
  assert.equal(receive(one, answer("q-2", of("x.rs"), "c-2")), one);
  assert.equal(receive(one, answer("q-2", of("x.rs"), "c-1", "w-2")), one);
  assert.equal(receive(two, { id: 0, type: "run", run: "r-1", event: { event: "text", text: "x" } }), two);
  assert.equal(receive(NO_CHANGES, answer("q-2", of("a.rs"))), NO_CHANGES);
  // Before any query, even an answer without a request is dropped.
  const unasked = { id: 0, type: "diff_answer", answer: of("a.rs") } as unknown as Envelope;
  assert.equal(receive(NO_CHANGES, unasked), NO_CHANGES);
});

test("unchanged counts only before any file, and a line this app can't number hides its file's lines", () => {
  const { changes } = ask(5, "q-1", "w-1", "c-1", NO_CHANGES);
  const one = receive(changes, answer("q-1", { kind: "file", files: 2, file: file("a.rs") }));
  const moved = { ...file("b.rs"), content: { kind: "text", hunks: [{ old_start: 1, new_start: 1, lines: [{ op: "moved", text: "x" }] }] } };

  assert.equal(receive(one, answer("q-1", { kind: "unchanged" })), one);
  assert.deepEqual(receive(one, answer("q-1", { kind: "file", files: 2, file: moved })).files[1], { ...moved, content: { kind: "newer" } });
});

test("an error under the query's id cuts the changes short, so opening them asks again", () => {
  const { changes } = ask(5, "q-1", "w-1", "c-1", NO_CHANGES);
  const error = (id: number): Envelope => ({ id, type: "error", code: "message.unsupported", retryable: false });
  const failed = receive(changes, error(5));

  assert.equal(needsAsking(NO_CHANGES), true);
  assert.equal(needsAsking(changes), false);
  assert.equal(failed.cut, true);
  assert.equal(needsAsking(failed), true);
  assert.equal(receive(changes, error(6)), changes);
  assert.equal(receive(NO_CHANGES, error(5)), NO_CHANGES);
});

test("asking again keeps the files shown until the new request's first answer replaces them", () => {
  const of = (path: string, files: number) => ({ kind: "file", files, file: file(path) });
  const { changes } = ask(5, "q-1", "w-1", "c-1", NO_CHANGES);
  const shown = cut(receive(changes, answer("q-1", of("a.rs", 2))));
  const again = ask(6, "q-2", "w-1", "c-1", shown).changes;
  const first = receive(again, answer("q-2", of("b.rs", 2)));
  const none = receive(again, answer("q-2", { kind: "unchanged" }));

  assert.deepEqual(again.files, shown.files);
  assert.equal(again.cut, false);
  assert.equal(receive(again, answer("q-1", of("old.rs", 2))), again);
  assert.deepEqual(first.files.map((each) => each.path), ["b.rs"]);
  assert.equal(first.held, false);
  assert.deepEqual({ files: none.files, total: none.total }, { files: [], total: 0 });
  assert.equal(cut(again).cut, true);
  assert.equal(ask(7, "q-3", "w-1", "c-1", NO_CHANGES).changes.held, false);
});

test("a connection that ends cuts short only changes still coming", () => {
  const { changes } = ask(5, "q-1", "w-1", "c-1", NO_CHANGES);
  const one = receive(changes, answer("q-1", { kind: "file", files: 2, file: file("a.rs") }));
  const all = receive(one, answer("q-1", { kind: "file", files: 2, file: file("b.rs") }));
  const none = receive(changes, answer("q-1", { kind: "unchanged" }));

  assert.equal(cut(changes).cut, true);
  assert.equal(cut(one).cut, true);
  assert.equal(cut(all), all);
  assert.equal(cut(none), none);
  assert.equal(cut(NO_CHANGES), NO_CHANGES);
});

test("changes can be asked for only on a live connection to a server that offers them", () => {
  const server = { name: "weacon-server", version: "0.1.0", commit: "abc" };

  assert.equal(canAsk({ state: "live", server, capabilities: ["run.stream", "diff.view"] }), true);
  assert.equal(canAsk({ state: "live", server, capabilities: ["run.stream"] }), false);
  assert.equal(canAsk({ state: "connecting" }), false);
  assert.equal(canAsk({ state: "ended", code: "connection.lost" }), false);
});

test("an unchanged chat has no files, and malformed answers are dropped", () => {
  const { changes } = ask(5, "q-1", "w-1", "c-1", NO_CHANGES);
  const text = (hunks: unknown) => ({ kind: "file", files: 1, file: { path: "a", change: { kind: "added" }, content: { kind: "text", hunks } } });
  const malformed = [
    "nope",
    null,
    { kind: "file", files: "1", file: file("a.rs") },
    { kind: "file", files: 1.5, file: file("a.rs") },
    { kind: "file", files: 0, file: file("a.rs") },
    { kind: "file", files: 1, file: { ...file("a.rs"), path: 7 } },
    { kind: "file", files: 1, file: { ...file("a.rs"), change: { kind: "renamed" } } },
    { kind: "file", files: 1, file: { ...file("a.rs"), change: "added" } },
    { kind: "file", files: 1, file: { ...file("a.rs"), content: { hunks: [] } } },
    text("none"),
    text([{ old_start: 1, new_start: "1", lines: [] }]),
    text([{ old_start: "1", new_start: 1, lines: [] }]),
    text([{ old_start: 1, new_start: 1, lines: "none" }]),
    text([{ old_start: 1, new_start: 1, lines: [{ op: "added" }] }]),
    text([{ old_start: 1, new_start: 1, lines: [{ text: "x" }] }]),
    text([{ old_start: 1, new_start: 1, lines: [null] }]),
  ];

  assert.deepEqual(receive(changes, answer("q-1", { kind: "unchanged" })), { ...changes, total: 0 });
  for (const body of malformed) {
    assert.equal(receive(changes, answer("q-1", body)), changes, JSON.stringify(body));
  }
  // A newer peer's kinds still arrive, for the view to show as it can.
  const renamed = { ...file("b.rs"), change: { kind: "renamed", from: "a.rs" } } as DiffFile;
  const newer = { ...file("c.rs"), change: { kind: "copied" }, content: { kind: "symlink" } } as unknown as DiffFile;
  for (const each of [renamed, newer]) {
    assert.deepEqual(receive(changes, answer("q-1", { kind: "file", files: 1, file: each })).files, [each]);
  }
});

test("each line is numbered on the sides it is on, after a row for its hunk", () => {
  const content = file("a.rs").content;
  assert.equal(content.kind, "text");

  assert.deepEqual(rows(content.kind === "text" ? content.hunks : []), [
    { kind: "hunk", old: 3, new: 5 },
    { kind: "line", op: "context", old: 3, new: 5, text: "a", noNewline: false },
    { kind: "line", op: "removed", old: 4, new: undefined, text: "b", noNewline: false },
    { kind: "line", op: "added", old: undefined, new: 6, text: "c", noNewline: false },
    { kind: "line", op: "added", old: undefined, new: 7, text: "d", noNewline: true },
  ]);
});
