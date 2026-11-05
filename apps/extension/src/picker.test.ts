import assert from "node:assert/strict";
import { test } from "node:test";

import { oneAtATime } from "./picker.ts";

test("a second pick while one is open joins it, and a later one starts afresh", async () => {
  let started = 0;
  let finish = (_: string) => {};
  const pick = oneAtATime(
    () =>
      new Promise<string>((resolve) => {
        started += 1;
        finish = resolve;
      }),
  );

  const first = pick();
  const second = pick();
  finish("button");

  assert.equal(first, second);
  assert.equal(await second, "button");
  void pick();
  assert.equal(started, 2);
});
