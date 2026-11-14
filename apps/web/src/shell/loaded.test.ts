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

/** `fetch`, noting `name` in `asked` when it starts. */
function named(
  asked: string[],
  name: string,
  fetch = async (): Promise<Answer<string[]>> => ({ ok: true, value: [name] }),
): () => Promise<Answer<string[]>> {
  return () => {
    asked.push(name);
    return fetch();
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

test("an answer never replaces a change made here", async () => {
  const { list, shown } = recorded();
  const pending = held<string[]>();
  const loading = list.load(pending.fetch);
  list.change((value) => [...(value ?? []), "made"]);
  pending.answer({ ok: true, value: ["before it"] });
  await loading;

  assert.deepEqual(
    shown.map(({ value }) => value),
    [["made"]],
  );
});

test("loads asked during a fetch run as one more fetch, once its answer is shown", async () => {
  const { list, shown } = recorded();
  const first = held<string[]>();
  const asked: string[] = [];
  const values = () => shown.map(({ value }) => value);
  const loading = list.load(named(asked, "first", first.fetch));
  const later = (name: string) =>
    named(asked, name, async () => {
      assert.deepEqual(values(), [["one"]]);
      return { ok: true, value: [name] };
    });
  const second = list.load(later("second"));
  const third = list.load(later("third"));
  first.answer({ ok: true, value: ["one"] });
  await Promise.all([loading, second, third]);
  await list.load(named(asked, "after"));

  assert.deepEqual(asked, ["first", "third", "after"]);
  assert.deepEqual(values(), [["one"], ["third"], ["after"]]);
});

test("a fetch that throws fails its load, and the load waiting behind it and the next one still fetch", async () => {
  const { list, shown } = recorded();
  const thrown = held<string[]>();
  const asked: string[] = [];
  const throwing = list.load(() => thrown.fetch().then(() => Promise.reject(new Error("thrown"))));
  const waiting = list.load(named(asked, "waiting"));
  thrown.answer({ ok: true, value: [] });
  await assert.rejects(throwing);
  await assert.rejects(waiting);
  await list.load(named(asked, "next"));

  assert.deepEqual(asked, ["waiting", "next"]);
  assert.deepEqual(
    shown.map(({ value }) => value),
    [["waiting"], ["next"]],
  );
});

test("after a reset, the next loads run one at a time and the old fetch takes none of them", async () => {
  const { list, shown } = recorded();
  const old = held<string[]>();
  const fresh = held<string[]>();
  const asked: string[] = [];
  const loading = list.load(old.fetch);
  void list.load(named(asked, "waiting"));
  list.reset();
  const started = list.load(named(asked, "fresh", fresh.fetch));
  const queued = list.load(named(asked, "queued"));
  old.answer({ ok: true, value: ["old"] });
  await loading;
  const latest = list.load(named(asked, "latest"));
  assert.deepEqual(asked, ["fresh"]);
  fresh.answer({ ok: true, value: ["fresh"] });
  await Promise.all([started, queued, latest]);

  assert.deepEqual(asked, ["fresh", "latest"]);
  assert.deepEqual(
    shown.map(({ value }) => value),
    [["fresh"], ["latest"]],
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
