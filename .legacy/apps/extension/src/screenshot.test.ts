import assert from "node:assert/strict";
import { test } from "node:test";

import { Failure } from "./core.ts";
import { base64, crop } from "./screenshot.ts";

test("base64 survives the chunks it is built in", () => {
  const bytes = Uint8Array.from({ length: 0x8000 * 2 + 3 }, (_, at) => at % 256);

  assert.equal(base64(bytes), Buffer.from(bytes).toString("base64"));
  assert.equal(base64(new Uint8Array()), "");
});

test("the crop is the element's visible part in capture pixels", () => {
  assert.deepEqual(crop({ x: 10, y: 20, width: 30, height: 40 }, 2, 1000, 800), { x: 20, y: 40, width: 60, height: 80 });
  assert.deepEqual(crop({ x: -10, y: 790, width: 30, height: 40 }, 1, 1000, 800), { x: 0, y: 790, width: 20, height: 10 });
  for (const rect of [
    { x: 1200, y: 0, width: 10, height: 10 },
    { x: 0, y: -50, width: 10, height: 40 },
    { x: 5, y: 5, width: 0, height: 10 },
  ]) {
    assert.throws(() => crop(rect, 1, 1000, 800), (error: unknown) => error instanceof Failure && error.code === "extension.element_hidden");
  }
});
