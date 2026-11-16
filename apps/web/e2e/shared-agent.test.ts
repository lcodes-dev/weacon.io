import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import { machine } from "./core.ts";
import { type Served, serve, signIn, signInAs } from "./server.ts";

// A page of its own browser context, signed in as `email`.
async function another(served: Served, email: string): Promise<Page> {
  const context = await served.page.context().browser()?.newContext();
  if (context === undefined) {
    throw new Error("no browser");
  }
  const page = await context.newPage();
  await page.goto(served.url);
  await signInAs(served, page, email);
  return page;
}

const box = (page: Page) => page.getByRole("combobox", { name: "Message" });

// Turns sharing of the machine `name` with the open workspace on or off, from its settings.
async function share(page: Page, name: string, shared: boolean): Promise<void> {
  const settings = page.getByRole("region", { name: `${name} settings` });
  // They open by themselves once for a machine just paired, which a click at that moment would undo.
  const opener = page.getByRole("button", { name: `${name} settings` });
  while ((await settings.count()) === 0) {
    if ((await opener.getAttribute("aria-expanded")) === "false") {
      await opener.click();
    }
    await settings.waitFor({ timeout: 2_000 }).catch(() => undefined);
  }
  const sharing = settings.getByLabel("Share this machine with this workspace");
  // Opened as the machine was paired, before its core was up, they are asked for again.
  const again = settings.getByRole("button", { name: "Try again" });
  await sharing.or(again).first().waitFor();
  if ((await again.count()) > 0) {
    await again.click();
  }
  // It shows the core's answer, so it changes once the core has taken the change.
  await sharing.click();
  await page.waitForFunction(([box, on]) => (box as HTMLInputElement).checked === on && !(box as HTMLInputElement).disabled, [await sharing.elementHandle(), shared] as const);
}

test("a member calls the agent of a machine shared with the workspace, and only its owner answers its prompts", { timeout: 900_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const ada = served.page;
  await signIn(served);
  await ada.getByLabel("Name").fill("Crew");
  await ada.getByRole("button", { name: "Create" }).click();
  await ada.getByRole("heading", { name: "Crew" }).waitFor();
  const workspace = new URL(ada.url()).searchParams.get("workspace");
  assert.ok(workspace !== null);
  await ada.getByRole("button", { name: "Settings", exact: true }).click();
  const members = ada.getByRole("region", { name: "Members" });
  await members.getByLabel("Email").fill("grace@example.com");
  await members.getByRole("button", { name: "Invite" }).click();
  await members.getByText("Invited grace@example.com.", { exact: false }).waitFor();
  const vps = await machine(served, workspace, "Team VPS");
  t.after(() => vps.close());
  await share(ada, "Team VPS", true);
  await ada.getByRole("button", { name: "Back" }).click();

  // Grace has no machine of her own.
  const grace = await another(served, "grace@example.com");
  await grace.getByRole("list", { name: "Invitations" }).getByRole("button", { name: "Accept" }).click();
  await grace.getByRole("heading", { name: "Crew" }).waitFor();

  await ada.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "New chat" }).click();
  // The box opens on her own agent's mention once the machine has answered; her words replace it.
  await ada.waitForFunction(() => document.querySelector("textarea")?.value === "@Claude Code ");
  await box(ada).fill("Shall we fix the parser?");
  await ada.getByRole("region", { name: "New chat" }).getByRole("button", { name: "Send" }).click();
  await ada.getByRole("heading", { name: "Shall we fix the parser?" }).waitFor();

  // Grace picks the shared machine as her agent's, marked as shared, and calls it.
  await grace.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "Shall we fix the parser?" }).click();
  await grace.getByRole("button", { name: "Add your agent" }).click();
  const fields = grace.getByRole("group", { name: "Your agent in this chat" });
  assert.deepEqual(await fields.getByLabel("Machine").getByRole("option").allInnerTexts(), ["Team VPS (shared)"]);
  await fields.getByLabel("Repository").selectOption("demo");
  await fields.getByLabel("Agent").selectOption("claude");
  await grace.waitForFunction(() => document.querySelector("textarea")?.value.startsWith("@Claude Code ") === true);
  await box(grace).press("End");
  await grace.keyboard.type("Run the parser tests");
  await grace.keyboard.press("Enter");

  // Its turn is the shared agent's, and its prompt waits for the machine's owner: Grace reads so, and can stop it.
  await grace.getByText("Shared Claude Code · Team VPS").waitFor();
  await grace.getByText("Waiting for ada").waitFor();
  assert.equal(await grace.getByRole("button", { name: "Allow" }).count(), 0);
  await grace.getByRole("button", { name: "Stop" }).waitFor();
  const [told] = vps.heard();
  assert.ok(told !== undefined && told.startsWith("@Claude Code Run the parser tests"), told);

  // Ada, who has no agent of her own in the chat, allows it, and the answer comes under the shared label.
  await ada.getByText("Shared Claude Code · Team VPS").waitFor();
  await ada.getByRole("button", { name: "Allow" }).click();
  await grace.getByText("All 12 tests pass.").waitFor();
  await grace.getByText("Done", { exact: true }).waitFor();
  await ada.getByText("All 12 tests pass.").waitFor();
  assert.equal(await grace.getByText("grace's Claude Code", { exact: false }).count(), 0);
  assert.equal(await grace.getByText("Allowed Bash").count(), 1);

  // Once Ada stops sharing it, it is no longer Grace's to call, at once: her box offers her own agent instead.
  await ada.getByRole("button", { name: "Settings", exact: true }).click();
  await share(ada, "Team VPS", false);
  await grace.getByRole("button", { name: "Add your agent" }).waitFor();
  assert.equal(vps.heard().length, 1);
});
