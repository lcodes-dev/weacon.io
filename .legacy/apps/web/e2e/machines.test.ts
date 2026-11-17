import assert from "node:assert/strict";
import { test } from "node:test";

import { claim, startPairing } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

test("a person pairs a machine by its code, then unpairs it", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByText("No machines yet. Run weacon pair on one, then enter the code it shows.").waitFor();

  await page.getByLabel("Code").fill("nothing-here");
  await page.getByRole("button", { name: "Look up" }).click();
  await page.getByText("No machine is waiting under that code. Check it, or run weacon pair again.").waitFor();

  const { pairing, code, secret } = await startPairing(page);
  await page.getByLabel("Code").fill(` ${code.toLowerCase()} `);
  await page.getByRole("button", { name: "Look up" }).click();
  await page.getByRole("heading", { name: "Pair Studio?" }).waitFor();
  assert.equal(await page.locator("dt:text-is('System') + dd").innerText(), "Linux");
  await page.getByRole("button", { name: "Confirm" }).click();
  await page.getByText("Waiting for the machine to connect.").waitFor();

  await claim(page, { pairing, secret });
  const machines = page.getByRole("list", { name: "Machines" });
  await machines.getByText("Studio").waitFor();
  await page.getByText("Waiting for the machine to connect.").waitFor({ state: "detached" });

  await page.reload();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  await row.getByText("Linux, paired").waitFor();
  await row.getByRole("button", { name: "Unpair" }).click();
  await row.getByText("Unpair Studio? It disconnects at once.").waitFor();
  await row.getByRole("button", { name: "Cancel" }).click();
  await page.route("**/api/machines/*", (route) => (route.request().method() === "DELETE" ? route.abort() : route.continue()));
  await row.getByRole("button", { name: "Unpair" }).click();
  await row.getByRole("button", { name: "Unpair" }).click();
  await row.getByRole("alert").getByText("The server can't be reached.").waitFor();
  await page.unroute("**/api/machines/*");
  await row.getByRole("button", { name: "Unpair" }).click();
  await page.getByText("No machines yet. Run weacon pair on one, then enter the code it shows.").waitFor();
});
