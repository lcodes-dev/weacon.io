import assert from "node:assert/strict";
import { test } from "node:test";

import { fakeServer, start } from "./harness.ts";

test("Settings shows this build and the server's, and the page's logs reach the server", { timeout: 120_000 }, async () => {
  const harness = await start(fakeServer(1_000, 0));
  try {
    const { page } = harness;
    await page.getByText("Connected").waitFor();
    await page.getByRole("button", { name: "Settings" }).click();
    const about = await page.locator("dl").innerText();
    // An uncaught error reaches the page's handler, which reports only its fixed code.
    await page.evaluate(() =>
      setTimeout(() => {
        throw new Error("MARKER-MESSAGE in /home/ada/MARKER-PATH with token MARKER-TOKEN");
      }),
    );
    await waitFor(() => harness.received().some((record) => JSON.stringify(record).includes("web.crashed")));
    const received = harness.received();

    assert.match(about, /Version\s+\d+\.\d+\.\d+\s+Commit\s+([0-9a-f]{7,64}|unknown)\s+Server\s+0\.0\.0 \(0000000\)/);
    assert.deepEqual(received[0], { id: 0, type: "log", level: "info", msg: "app started" });
    assert.doesNotMatch(JSON.stringify(received), /MARKER/);
  } finally {
    await harness.close();
  }
});

// Polls, since the server writes what it receives to a file.
async function waitFor(done: () => boolean, deadlineMs = 10_000): Promise<void> {
  const end = Date.now() + deadlineMs;
  while (!done()) {
    if (Date.now() > end) {
      throw new Error("gave up waiting");
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}
