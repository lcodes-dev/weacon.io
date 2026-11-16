import assert from "node:assert/strict";
import { test } from "node:test";

import { Departures } from "./departures.ts";

test("leaving a workspace is news once, whatever others are left meanwhile", () => {
  const departures = new Departures();

  assert.equal(departures.leave("w-1"), true);
  assert.equal(departures.leave("w-1"), false);
  assert.equal(departures.leave("w-2"), true);
  assert.equal(departures.leave("w-1"), false);
});

test("leaving a workspace the list names again is news once more", () => {
  const departures = new Departures();
  departures.leave("w-1");
  departures.leave("w-2");

  departures.listed(["w-1", "w-3"]);

  assert.equal(departures.leave("w-1"), true);
  assert.equal(departures.leave("w-2"), false);
});
