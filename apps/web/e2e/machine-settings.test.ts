import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSettings } from "../src/protocol.ts";
import { core, pair } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

const START: MachineSettings = {
  access: "ask_first",
  team: false,
  sign_ins: [
    { agent: "claude", state: "signed_in" },
    { agent: "codex", state: "not_installed" },
  ],
};

test("a person reads and changes a machine's settings for the open workspace, through its core", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Settings" }).click();
  const token = await pair(page);
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  await row.waitFor();
  // With no workspace open, a machine's settings have no workspace to be for.
  assert.equal(await row.getByRole("button", { name: "Studio settings" }).count(), 0);

  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings" }).click();
  await row.getByRole("button", { name: "Studio settings" }).click();
  await row.getByText("This machine isn't connected. Start Weacon on it, then try again.").waitFor();

  const machine = await core(page, token, START);
  t.after(() => machine.close());
  await row.getByRole("button", { name: "Try again" }).click();
  const settings = row.getByRole("region", { name: "Studio settings" });
  await settings.getByText("Not chosen yet, so none are sent.").waitFor();
  const signIns = await settings.getByRole("list", { name: "Sign-ins" }).getByRole("listitem").allInnerTexts();
  assert.deepEqual(
    signIns.map((text) => text.trim()),
    ["Claude Code: Signed in", "Codex: Not installed"],
  );

  const warning = settings.getByText("Full access is on in a team workspace. Its agents change files on this machine without asking.");
  const team = settings.getByLabel("Other people are in this workspace");
  const level = settings.getByLabel("What this workspace's agents may do on this machine");

  await settings.getByRole("button", { name: "Keep off" }).click();
  await settings.getByText("Off.", { exact: true }).waitFor();
  assert.equal(machine.settings.reports, false);
  await settings.getByRole("button", { name: "Turn on" }).click();
  await settings.getByText("On.", { exact: true }).waitFor();
  assert.equal(await settings.getByRole("button", { name: "Turn on" }).count(), 0);

  // The warning is for full access in a team workspace only.
  await level.selectOption("full");
  await settings.getByRole("button", { name: "Turn off" }).waitFor();
  assert.equal(machine.settings.access, "full");
  assert.equal(await warning.count(), 0);
  // The box shows what the core holds, so it ticks once the core has answered.
  await team.click();
  await warning.waitFor();
  assert.ok(await team.isChecked());
  await level.selectOption("ask_first");
  await warning.waitFor({ state: "detached" });
  assert.equal(machine.settings.team, true);

  machine.refuse();
  await settings.getByRole("button", { name: "Turn off" }).click();
  await settings.getByText("Only this machine's owner can see or change its settings.").waitFor();
  await settings.getByText("On.", { exact: true }).waitFor();
  assert.deepEqual({ ...machine.settings, sign_ins: [] }, { access: "ask_first", team: true, reports: true, sign_ins: [] });
});
