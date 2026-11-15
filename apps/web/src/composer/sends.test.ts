import assert from "node:assert/strict";
import { test } from "node:test";

import { Sends } from "./sends.ts";

function fresh() {
  let next = 0;
  return () => `q-${++next}`;
}

const UNSURE = "The server didn't answer. The message may not have been sent.";

test("a taken send is taken at once; one that may have run goes again under its request, an edited one under a new one", () => {
  const sends = new Sends<string>();
  const ids = fresh();
  const first = sends.start("hi", "one", ids);

  assert.deepEqual(sends.settle(first, "unsure"), { payload: "one", taken: false, latest: true, problem: UNSURE });
  assert.equal(sends.start("hi", "two", ids), first);
  assert.deepEqual(sends.settle(first, "accepted"), { payload: "two", taken: true, latest: true, problem: undefined });
  assert.equal(sends.start("hi", "three", ids), "q-2");
  sends.settle("q-2", "lost");
  assert.equal(sends.start("hi!", "four", ids), "q-3");
});

test("an older send's late receipt still settles it once another was sent, and once only", () => {
  const sends = new Sends<string>();
  const ids = fresh();
  const a = sends.start("A", "a", ids);
  sends.settle(a, "unsure");
  const b = sends.start("B", "b", ids);
  sends.settle(b, "refused");

  assert.deepEqual(sends.settle(a, "accepted"), { payload: "a", taken: true, latest: false, problem: undefined });
  assert.equal(sends.settle(a, "accepted"), undefined);
  assert.equal(sends.settle("q-9", "accepted"), undefined);
});

test("a late acceptance while a retry of the same request is out settles it, and the retry's own answer then counts for nothing", () => {
  const sends = new Sends<string>();
  const ids = fresh();
  const first = sends.start("hi", "one", ids);
  sends.settle(first, "unsure");
  assert.equal(sends.start("hi", "two", ids), first);

  assert.deepEqual(sends.settle(first, "accepted"), { payload: "two", taken: true, latest: true, problem: undefined });
  assert.equal(sends.settle(first, "unsure"), undefined);
  // Nothing of it is kept for later.
  assert.equal(sends.start("hi", "three", ids), "q-2");
});

test("a refusal, on time or late, ends the send without taking it, and a send too long to go is forgotten", () => {
  const sends = new Sends<string>();
  const ids = fresh();
  const first = sends.start("hi", "one", ids);
  sends.settle(first, "lost");

  assert.deepEqual(sends.settle(first, "refused"), { payload: "one", taken: false, latest: true, problem: "The message wasn't sent." });
  assert.equal(sends.start("hi", "two", ids), "q-2");
  assert.equal(sends.settle("q-2", "too_long")?.taken, false);
  assert.equal(sends.start("hi", "three", ids), "q-3");
});

test("a text sent again after another goes under the request it may have run under", () => {
  const sends = new Sends<string>();
  const ids = fresh();
  const a = sends.start("A", "a", ids);
  sends.settle(a, "unsure");
  const b = sends.start("B", "b", ids);
  sends.settle(b, "refused");

  assert.equal(sends.start("A", "a again", ids), a);
});
