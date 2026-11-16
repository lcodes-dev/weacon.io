import assert from "node:assert/strict";
import { test } from "node:test";

import { fakeServer, start } from "./harness.ts";

test("Settings shows this build and the server's", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0));
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByText("Connected").waitFor();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const about = await page.locator("dl").innerText();

  assert.match(about, /Version\s+\d+\.\d+\.\d+\s+Commit\s+([0-9a-f]{7,64}|unknown)\s+Server\s+0\.0\.0 \(0000000\)/);
});
