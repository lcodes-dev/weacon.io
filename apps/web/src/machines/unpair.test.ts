import assert from "node:assert/strict";
import { test } from "node:test";

import type { Answer } from "../shell/api.ts";
import { unpair } from "./unpair.ts";

const answering = (answer: Answer<undefined>) => ({ unpair: async () => answer });

test("a machine already unpaired elsewhere counts as unpaired, and other failures stay failures", async () => {
  const done = { ok: true, value: undefined } as const;

  assert.deepEqual(await unpair(answering(done), "mc-1"), done);
  assert.deepEqual(await unpair(answering({ ok: false, code: "machine.not_found" }), "mc-1"), done);
  assert.deepEqual(await unpair(answering({ ok: false, code: "connection.refused" }), "mc-1"), {
    ok: false,
    code: "connection.refused",
  });
});
