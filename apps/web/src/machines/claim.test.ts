import assert from "node:assert/strict";
import { test } from "node:test";

import type { Later } from "../connection.ts";
import { GIVE_UP_MS, LOOK_AGAIN_MS, waitForClaim } from "./claim.ts";

/** A clock the test moves by hand: `tick(ms)` runs what was due by then, in order. */
function clock(): { later: Later; tick(ms: number): Promise<void> } {
  let now = 0;
  const due: Array<{ at: number; run: () => void }> = [];
  return {
    later: (run, ms) => {
      due.push({ at: now + ms, run });
    },
    tick: async (ms) => {
      const until = now + ms;
      for (;;) {
        due.sort((one, other) => one.at - other.at);
        const next = due[0];
        if (next === undefined || next.at > until) {
          now = until;
          return;
        }
        due.shift();
        now = next.at;
        next.run();
        // Lets a look's answer settle, so the next one is scheduled.
        await Promise.resolve();
        await Promise.resolve();
      }
    },
  };
}

test("the list is read again every two seconds, each time after the last answer, until stopped", async () => {
  const { later, tick } = clock();
  let looks = 0;
  let answer: () => void = () => {};
  const look = () => {
    looks++;
    return new Promise<void>((resolve) => {
      answer = resolve;
    });
  };
  const stop = waitForClaim(look, later, () => assert.fail("gave up"));

  await tick(LOOK_AGAIN_MS - 1);
  assert.equal(looks, 0);
  await tick(1);
  assert.equal(looks, 1);
  await tick(LOOK_AGAIN_MS * 5);
  assert.equal(looks, 1, "a slow answer isn't cut short by the next look");
  answer();
  await tick(0);
  await tick(LOOK_AGAIN_MS);
  assert.equal(looks, 2);
  stop();
  answer();
  await tick(GIVE_UP_MS);
  assert.equal(looks, 2);
});

test("after two minutes without being stopped it gives up once and looks no more", async () => {
  const { later, tick } = clock();
  let looks = 0;
  let gaveUp = 0;
  waitForClaim(
    async () => {
      looks++;
    },
    later,
    () => gaveUp++,
  );

  await tick(GIVE_UP_MS - 1);
  assert.equal(gaveUp, 0);
  const before = looks;
  await tick(1);
  await tick(LOOK_AGAIN_MS * 3);

  assert.equal(gaveUp, 1);
  assert.ok(before > 50, `${before}`);
  assert.equal(looks, before);
});
