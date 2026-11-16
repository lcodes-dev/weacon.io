import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSettings } from "../src/protocol.ts";
import { core, pair } from "./pairing.ts";
import { waitFor } from "./harness.ts";
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
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const { token } = await pair(page);
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  await row.waitFor();
  // With no workspace open, a machine's settings have no workspace to be for.
  assert.equal(await row.getByRole("button", { name: "Studio settings" }).count(), 0);

  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  // Paired with no workspace open, so they don't open by themselves once one is.
  await row.getByRole("button", { name: "Studio settings" }).waitFor();
  assert.equal(await row.getByRole("region", { name: "Studio settings" }).count(), 0);
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
    ["Claude Code: Signed in Sign in again", "Codex: Not installed"],
  );
  // This core opens no sign-in terminals, so the server closes the one asked for.
  await settings.getByRole("button", { name: "Sign in again" }).click();
  await settings.getByText("The machine isn't connected, or its core can't open sign-in terminals.").waitFor();
  // xterm's own rules apply under the server's policy, which blocks inline styles.
  const rows = settings.locator(".xterm-rows");
  assert.equal(await rows.evaluate((element) => getComputedStyle(element).color), "rgb(255, 255, 255)");
  // The sign-ins are asked again once it closes, and again once the person closes it.
  await waitFor(() => machine.queries === 2);
  machine.set({ ...machine.settings, sign_ins: [{ agent: "claude", state: "signed_out" }, { agent: "codex", state: "not_installed" }] });
  await settings.getByRole("button", { name: "Close the sign-in" }).click();
  await settings.getByRole("button", { name: "Sign in", exact: true }).waitFor();
  machine.set({ ...machine.settings, sign_ins: START.sign_ins });

  const warning = settings.getByText("Full access is on in a team workspace. Its agents change files on this machine without asking.");
  const level = settings.getByLabel("What this workspace's agents may do on this machine");

  await settings.getByRole("button", { name: "Keep off" }).click();
  await settings.getByText("Off.", { exact: true }).waitFor();
  assert.equal(machine.settings.reports, false);
  await settings.getByRole("button", { name: "Turn on" }).click();
  await settings.getByText("On.", { exact: true }).waitFor();
  assert.equal(await settings.getByRole("button", { name: "Turn on" }).count(), 0);

  // The warning is for full access in a team workspace only: alone, there is none.
  await level.selectOption("full");
  await waitFor(() => machine.settings.access === "full");
  // The page shows the core's answer before the next change is built from it.
  await page.waitForFunction((select) => (select as HTMLSelectElement | null)?.value === "full", await level.elementHandle());
  assert.equal(await warning.count(), 0);
  assert.equal(machine.settings.team, false);
  // With someone else in the workspace it shows, and the next level saved tells the machine so.
  await page.route("**/api/workspaces/*/members", async (route) => {
    const answer = await route.fetch();
    const list = (await answer.json()) as { members: Array<{ id: string; name: string; role?: string }> };
    await route.fulfill({ response: answer, json: { ...list, members: [...list.members, { id: "m-other", name: "bo", role: "member" }] } });
  });
  await page.reload();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await row.getByRole("button", { name: "Studio settings" }).click();
  await warning.waitFor();
  assert.equal(machine.settings.team, false);
  await level.selectOption("ask_first");
  await warning.waitFor({ state: "detached" });
  await waitFor(() => machine.settings.team);

  machine.refuse();
  await settings.getByRole("button", { name: "Turn off" }).click();
  await settings.getByText("Only this machine's owner can see or change its settings.").waitFor();
  await settings.getByText("On.", { exact: true }).waitFor();
  assert.deepEqual({ ...machine.settings, sign_ins: [] }, { access: "ask_first", team: true, reports: true, sign_ins: [] });
});

test("a machine paired with a workspace open shows its settings at once, to ask about error reports", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  // Holds the machine list until its core is connected, so the settings open on a core they can reach.
  const connected = Promise.withResolvers<void>();
  await page.route("**/api/machines", async (route) => {
    await connected.promise;
    await route.continue();
  });
  const { token } = await pair(page);
  const machine = await core(page, token, START);
  t.after(() => machine.close());
  connected.resolve();
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });

  // Nobody opened them; they ask until the person answers, and nothing is sent meanwhile.
  const settings = row.getByRole("region", { name: "Studio settings" });
  await settings.getByText("Not chosen yet, so none are sent.").waitFor();
  assert.equal(machine.settings.reports, undefined);
  await settings.getByRole("button", { name: "Keep off" }).click();
  await settings.getByText("Off.", { exact: true }).waitFor();
  assert.equal(machine.settings.reports, false);
});
