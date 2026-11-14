import assert from "node:assert/strict";
import { test } from "node:test";

import { Sends } from "./sends.ts";

function fresh() {
  let next = 0;
  return () => `q-${++next}`;
}

const UNSURE = "The server didn't answer. The message may not have been sent.";

test("a taken send is taken at once; one that may have run goes again under its request, an edited one under a new one", () => {
  const sends = new Sends();
  const ids = fresh();
  const first = sends.start("hi", ids);

  assert.deepEqual(sends.end("hi", first, "unsure"), { text: "hi", taken: false, latest: true, problem: UNSURE });
  assert.equal(sends.start("hi", ids), first);
  assert.deepEqual(sends.end("hi", first, "accepted"), { text: "hi", taken: true, latest: true, problem: undefined });
  assert.equal(sends.start("hi", ids), "q-2");
  sends.end("hi", "q-2", "lost");
  assert.equal(sends.start("hi!", ids), "q-3");
});

test("an older send's late receipt still settles it once another was sent, and leaves the latest's problem alone", () => {
  const sends = new Sends();
  const ids = fresh();
  const a = sends.start("A", ids);
  sends.end("A", a, "unsure");
  const b = sends.start("B", ids);
  sends.end("B", b, "refused");

  assert.deepEqual(sends.late({ request: a, settled: "accepted" }), { text: "A", taken: true, latest: false, problem: undefined });
  assert.equal(sends.late({ request: a, settled: "accepted" }), undefined);
  assert.equal(sends.late({ request: "q-9", settled: "accepted" }), undefined);
});

test("a late acceptance while a retry of the same request is out makes the retry's own answer count as taken", () => {
  const sends = new Sends();
  const ids = fresh();
  const first = sends.start("hi", ids);
  sends.end("hi", first, "unsure");
  assert.equal(sends.start("hi", ids), first);

  assert.deepEqual(sends.late({ request: first, settled: "accepted" }), { text: "hi", taken: true, latest: true, problem: undefined });
  assert.deepEqual(sends.end("hi", first, "unsure"), { text: "hi", taken: true, latest: true, problem: undefined });
  // Nothing of it is kept for later.
  assert.equal(sends.start("hi", ids), "q-2");
});

test("a late refusal ends the wait without taking the text", () => {
  const sends = new Sends();
  const ids = fresh();
  const first = sends.start("hi", ids);
  sends.end("hi", first, "lost");

  assert.deepEqual(sends.late({ request: first, settled: "refused" }), {
    text: "hi",
    taken: false,
    latest: true,
    problem: "The message wasn't sent.",
  });
  assert.equal(sends.start("hi", ids), "q-2");
});

test("a text sent again after another goes under the request it may have run under", () => {
  const sends = new Sends();
  const ids = fresh();
  const a = sends.start("A", ids);
  sends.end("A", a, "unsure");
  const b = sends.start("B", ids);
  sends.end("B", b, "refused");

  assert.equal(sends.start("A", ids), a);
});
