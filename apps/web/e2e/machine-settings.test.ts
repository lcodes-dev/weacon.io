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
  assert.equal(await row.getByRole("button", { name: "Settings" }).count(), 0);

  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings" }).click();
  await row.getByRole("button", { name: "Settings" }).click();
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

  await settings.getByRole("button", { name: "Turn on" }).click();
  await settings.getByText("On. They hold no messages, prompts, code or file contents.").waitFor();
  await settings.getByLabel("What this workspace's agents may do on this machine").selectOption("full");
  // The box shows what the core holds, so it ticks once the core has answered.
  await settings.getByLabel("Other people are in this workspace").click();
  await settings.getByText("Full access is on in a team workspace. Its agents change files on this machine without asking.").waitFor();
  assert.ok(await settings.getByLabel("Other people are in this workspace").isChecked());

  assert.deepEqual({ ...machine.settings, sign_ins: [] }, { access: "full", team: true, reports: true, sign_ins: [] });
});
