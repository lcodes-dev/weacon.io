import assert from "node:assert/strict";
import { test } from "node:test";

import type { Answer } from "./api.ts";
import { type Loaded, Loader } from "./loaded.ts";

/** A fetch whose answer comes when the test says. */
function held<T>(): { fetch: () => Promise<Answer<T>>; answer(answer: Answer<T>): void } {
  let answer: (answer: Answer<T>) => void = () => {};
  return {
    fetch: () =>
      new Promise((resolve) => {
        answer = resolve;
      }),
    answer: (given) => answer(given),
  };
}

function recorded(): { list: Loader<string[]>; shown: Array<Loaded<string[]>> } {
  const shown: Array<Loaded<string[]>> = [];
  return { list: new Loader((loaded) => shown.push(loaded)), shown };
}

test("a fetch shows its value, a failed one keeps it with the problem, and trying again clears the problem first", async () => {
  const { list, shown } = recorded();
  await list.load(async () => ({ ok: true, value: ["a"] }));
  await list.load(async () => ({ ok: false, code: "connection.refused" }));
  await list.load(async () => ({ ok: true, value: ["a", "b"] }));

  assert.deepEqual(shown, [
    { value: ["a"], problem: undefined },
    { value: ["a"], problem: "The server can't be reached" },
    { value: ["a"], problem: undefined },
    { value: ["a", "b"], problem: undefined },
  ]);
});

test("an answer to an older fetch never replaces a newer one, or a change made here", async () => {
  const { list, shown } = recorded();
  const first = held<string[]>();
  const second = held<string[]>();
  const slow = list.load(first.fetch);
  const fast = list.load(second.fetch);
  second.answer({ ok: true, value: ["new"] });
  await fast;
  first.answer({ ok: true, value: ["old"] });
  await slow;
  const third = held<string[]>();
  const late = list.load(third.fetch);
  list.change((value) => [...(value ?? []), "made"]);
  third.answer({ ok: true, value: ["before it"] });
  await late;

  assert.deepEqual(
    shown.map(({ value }) => value),
    [["new"], ["new", "made"]],
  );
});

test("once stopped, answers still on their way are dropped", async () => {
  const { list, shown } = recorded();
  const pending = held<string[]>();
  const loading = list.load(pending.fetch);
  list.stop();
  pending.answer({ ok: false, code: "connection.refused" });
  await loading;

  assert.deepEqual(shown, []);
});

test("a reset forgets what is shown and drops answers on their way", async () => {
  const { list, shown } = recorded();
  await list.load(async () => ({ ok: true, value: ["a"] }));
  const pending = held<string[]>();
  const loading = list.load(pending.fetch);
  list.reset();
  list.reset();
  pending.answer({ ok: true, value: ["late"] });
  await loading;

  assert.deepEqual(shown, [
    { value: ["a"], problem: undefined },
    { value: undefined, problem: undefined },
  ]);
});
