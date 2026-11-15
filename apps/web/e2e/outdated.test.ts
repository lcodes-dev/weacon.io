import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSettings } from "../src/protocol.ts";
import { BUILD, core, dial, pair } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

const SETTINGS: MachineSettings = { access: "ask_first", team: false, sign_ins: [] };
const CURRENT = ["machine.connect", "machine.settings", "diff.view", "worktree.manage", "sessions.read", "workspace.proposals"];

test("a core older than the server shows as outdated in the machines list and its settings, and a current one doesn't", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings" }).click();
  const { token } = await pair(page);
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  // The list is read again only when the page is.
  const reopen = async () => {
    await page.reload();
    await page.getByRole("button", { name: "Settings" }).click();
    await row.getByText("Linux, paired").waitFor();
  };
  const outdated = row.getByText("Its weacon is older than this server supports. Run weacon update there.");
  await reopen();
  assert.equal(await row.getByText("weacon 0.0.0").count(), 0);
  assert.equal(await row.getByRole("status").count(), 0);

  const older = await core(page, token, SETTINGS);
  await reopen();
  await row.getByText(`weacon ${BUILD.version}`).waitFor();
  await outdated.waitFor();
  await row.getByRole("button", { name: "Studio settings" }).click();
  const settings = row.getByRole("region", { name: "Studio settings" });
  await settings.getByText("Its weacon is older than this server supports, so some settings may not reach it.").waitFor();
  older.close();

  const current = dial(page, token, { ...BUILD, version: "0.0.1" }, CURRENT);
  t.after(() => current.close());
  await new Promise<void>((resolve) => {
    current.onmessage = ({ data }) => JSON.parse(String(data)).type === "welcome" && resolve();
  });
  await reopen();
  await row.getByText("weacon 0.0.1").waitFor();
  await row.getByRole("button", { name: "Studio settings" }).click();
  await settings.waitFor();
  assert.equal(await row.getByRole("status").count(), 0);
});
