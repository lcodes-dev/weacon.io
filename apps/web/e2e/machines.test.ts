import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSummary } from "../src/protocol.ts";
import { FINGERPRINT, KEY, claim, pair, pinned, setPin, startPairing } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

test("a person pairs a machine by its code after checking its fingerprint, then unpairs it", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByText("No machines yet. Run weacon pair on one, then enter the code it shows.").waitFor();

  await page.getByLabel("Code").fill("nothing-here");
  await page.getByRole("button", { name: "Look up" }).click();
  await page.getByText("No machine is waiting under that code. Check it, or run weacon pair again.").waitFor();

  const { pairing, code, secret } = await startPairing(page);
  await page.getByLabel("Code").fill(` ${code.toLowerCase()} `);
  await page.getByRole("button", { name: "Look up" }).click();
  await page.getByRole("heading", { name: "Pair Studio?" }).waitFor();
  assert.equal(await page.locator("dt:text-is('Fingerprint') + dd").innerText(), FINGERPRINT);
  assert.equal(await page.locator("dt:text-is('System') + dd").innerText(), "Linux");
  await page.getByRole("button", { name: "Confirm" }).click();
  await page.getByText("Waiting for the machine to connect.").waitFor();

  const paired = await claim(page, { pairing, secret });
  const machines = page.getByRole("list", { name: "Machines" });
  await machines.getByText("Studio").waitFor();
  await page.getByText("Waiting for the machine to connect.").waitFor({ state: "detached" });
  assert.equal(await pinned(page, paired.machine), KEY);

  await page.reload();
  await page.getByRole("button", { name: "Settings" }).click();
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  await row.getByText("Linux, paired").waitFor();
  await row.getByRole("button", { name: "Unpair" }).click();
  await row.getByText("Unpair Studio? It disconnects at once.").waitFor();
  await row.getByRole("button", { name: "Cancel" }).click();
  await page.route("**/api/machines/*", (route) => (route.request().method() === "DELETE" ? route.abort() : route.continue()));
  await row.getByRole("button", { name: "Unpair" }).click();
  await row.getByRole("button", { name: "Unpair" }).click();
  await row.getByRole("alert").getByText("The server can't be reached.").waitFor();
  assert.equal(await pinned(page, paired.machine), KEY);
  await page.unroute("**/api/machines/*");
  await row.getByRole("button", { name: "Unpair" }).click();
  await page.getByText("No machines yet. Run weacon pair on one, then enter the code it shows.").waitFor();
  assert.equal(await pinned(page, paired.machine), undefined);
});

test("a confirmed key that isn't the one shown is never pinned", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings" }).click();
  const { code } = await startPairing(page);
  const other = "cd".repeat(32);
  const swapped: MachineSummary = { id: "mc-swapped", name: "Studio", os: "linux", key: other, paired_at: 1_763_000_000 };
  await page.route("**/api/machines/pair/confirm", async (route) => {
    await route.fetch();
    await route.fulfill({ status: 201, json: swapped });
  });

  await page.getByLabel("Code").fill(code);
  await page.getByRole("button", { name: "Look up" }).click();
  await page.getByRole("button", { name: "Confirm" }).click();
  await page.getByText("The machine's key isn't the one you checked. If it shows in your list, unpair it.").waitFor();

  assert.equal(await page.getByRole("button", { name: "Confirm" }).count(), 0);
  assert.equal(await pinned(page, swapped.id), undefined);
  await page.getByRole("button", { name: "Close" }).click();
  await page.getByRole("button", { name: "Look up" }).waitFor();
});

test("another browser trusts a machine's key once the person checks its fingerprint there", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings" }).click();
  const { machine } = await pair(page);
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  await row.waitFor();
  const step = row.getByRole("region", { name: "Confirm Studio's key" });
  // The browser that paired it trusts it already.
  assert.equal(await pinned(page, machine), KEY);
  assert.equal(await step.count(), 0);

  await setPin(page, machine, undefined);
  await page.reload();
  await page.getByRole("button", { name: "Settings" }).click();
  await step.getByText(FINGERPRINT).waitFor();
  assert.equal(await pinned(page, machine), undefined);
  await step.getByRole("button", { name: "It matches" }).click();
  await step.waitFor({ state: "detached" });
  assert.equal(await pinned(page, machine), KEY);

  // A browser that trusts another key never offers to replace it.
  await setPin(page, machine, "cd".repeat(32));
  await page.reload();
  await page.getByRole("button", { name: "Settings" }).click();
  await row.getByText("This browser trusts another key for Studio. If you didn't pair it again, unpair it.").waitFor();
  assert.equal(await step.count(), 0);
});
