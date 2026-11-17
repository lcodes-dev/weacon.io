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

const SHARED = "@Shared Claude Code · Team VPS ";

// Waits for the message box to open on the shared agent's mention, then writes `text` after it.
async function call(page: Page, text: string): Promise<void> {
  await page.waitForFunction((opened) => document.querySelector("textarea")?.value === opened, SHARED);
  await box(page).press("End");
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

test("two members call the one agent of a machine shared with their workspace, and only its owner answers its prompts", { timeout: 900_000 }, async (t) => {
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
  // The box opens on the shared agent's mention once the machine has answered; her words replace it.
  await ada.waitForFunction((opened) => document.querySelector("textarea")?.value === opened, SHARED);
  await box(ada).fill("Shall we fix the parser?");
  await ada.getByRole("region", { name: "New chat" }).getByRole("button", { name: "Send" }).click();
  await ada.getByRole("heading", { name: "Shall we fix the parser?" }).waitFor();

  // Grace picks the shared machine, marked as shared, and calls its agent.
  await grace.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "Shall we fix the parser?" }).click();
  await grace.getByRole("button", { name: "Add your agent" }).click();
  const fields = grace.getByRole("group", { name: "Your agent in this chat" });
  assert.deepEqual(await fields.getByLabel("Machine").getByRole("option").allInnerTexts(), ["Team VPS (shared)"]);
  await fields.getByLabel("Repository").selectOption("demo");
  await fields.getByLabel("Agent").selectOption("claude");
  await call(grace, "Run the parser tests");

  // Its turn is the shared agent's, and its prompt waits for the machine's owner: Grace reads so, and can stop it.
  await grace.getByText("Shared Claude Code · Team VPS").first().waitFor();
  await grace.getByText("Waiting for ada").waitFor();
  assert.equal(await grace.getByRole("button", { name: "Allow" }).count(), 0);
  await grace.getByRole("button", { name: "Stop" }).waitFor();
  // What she writes next stays in the box as the chat's list catches up with her agent.
  await grace.waitForFunction((opened) => document.querySelector("textarea")?.value === opened, SHARED);
  await box(grace).press("End");
  await grace.keyboard.type("and the docs");
  await fields.waitFor({ state: "detached" });
  assert.equal(await box(grace).inputValue(), `${SHARED}and the docs`);
  await box(grace).fill("");

  // Ada allows it, and the answer comes under the shared label.
  await ada.getByRole("button", { name: "Allow" }).click();
  await grace.getByText("All 12 tests pass.").waitFor();
  await grace.getByText("Done", { exact: true }).waitFor();
  await ada.getByText("All 12 tests pass.").waitFor();
  assert.equal(await grace.getByText("grace's Claude Code", { exact: false }).count(), 0);
  assert.equal(await grace.getByText("Allowed Bash").count(), 1);

  // Ada calls the same agent: the chat has one on that machine, which her box offers as the shared one.
  await box(ada).focus();
  await ada.keyboard.type("@sh");
  const offered = ada.getByRole("listbox", { name: "Agents" }).getByRole("option");
  await offered.first().waitFor();
  assert.deepEqual(await offered.allInnerTexts(), ["Shared Claude Code · Team VPS\nShared agent"]);
  await ada.keyboard.press("Enter");
  await ada.keyboard.type("And the linter");
  await ada.keyboard.press("Enter");
  await grace.getByText(`${SHARED}And the linter`).waitFor();
  await ada.getByRole("button", { name: "Allow" }).click();
  await grace.getByText("Done", { exact: true }).nth(1).waitFor();
  const [first, second] = vps.heard();
  assert.equal(vps.heard().length, 2);
  // The agent read Ada's message as a quote with Grace's call, and nothing it already has with Ada's.
  assert.ok(first?.startsWith(`${SHARED}Run the parser tests`) && first.includes("Shall we fix the parser?"), first);
  assert.equal(second, `${SHARED}And the linter`);

  // Once Ada stops sharing the machine, its agent is no longer Grace's to call, at once.
  await grace.getByPlaceholder("Write to the chat, or type @ to call Shared Claude Code · Team VPS").waitFor();
  await ada.getByRole("button", { name: "Settings", exact: true }).click();
  await share(ada, "Team VPS", false);
  await grace.getByPlaceholder("Write to the chat", { exact: true }).waitFor();
  assert.equal(vps.heard().length, 2);
});

test("a shared agent someone else calls leaves the agent a member is picking as it was", { timeout: 900_000 }, async (t) => {
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
  const grace = await another(served, "grace@example.com");
  await grace.getByRole("list", { name: "Invitations" }).getByRole("button", { name: "Accept" }).click();
  await grace.getByRole("heading", { name: "Crew" }).waitFor();
  await grace.getByRole("button", { name: "Settings", exact: true }).click();
  const desk = await machine({ ...served, page: grace }, workspace, "Desk");
  t.after(() => desk.close());
  await grace.getByRole("button", { name: "Back" }).click();
  await ada.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "New chat" }).click();
  await ada.waitForFunction((opened) => document.querySelector("textarea")?.value === opened, SHARED);
  await box(ada).fill("Shall we fix the parser?");
  await ada.getByRole("region", { name: "New chat" }).getByRole("button", { name: "Send" }).click();
  await ada.getByRole("heading", { name: "Shall we fix the parser?" }).waitFor();

  // Grace picks her own machine, of the two she may call.
  await grace.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "Shall we fix the parser?" }).click();
  await grace.getByRole("button", { name: "Add your agent" }).click();
  const fields = grace.getByRole("group", { name: "Your agent in this chat" });
  const machines = fields.getByLabel("Machine").getByRole("option");
  await fields.getByLabel("Machine").selectOption({ label: "Desk" });
  await fields.getByLabel("Repository").selectOption("demo");
  await fields.getByLabel("Agent").selectOption("claude");
  await grace.waitForFunction(() => document.querySelector("textarea")?.value === "@Claude Code ");
  assert.equal(await machines.count(), 2);

  // Ada calls the shared machine's agent meanwhile, which Grace's chat then has.
  await ada.getByRole("button", { name: "Add your agent" }).click();
  const hers = ada.getByRole("group", { name: "Your agent in this chat" });
  await hers.getByLabel("Repository").selectOption("demo");
  await hers.getByLabel("Agent").selectOption("claude");
  await call(ada, "Run the parser tests");
  await ada.getByRole("button", { name: "Allow" }).click();
  await grace.getByText("Done", { exact: true }).waitFor();
  await grace.waitForFunction(() => document.querySelector(".my-agent select")?.querySelectorAll("option").length !== 2);

  // Her pick stands, and her call goes to her own agent.
  assert.deepEqual(await machines.allInnerTexts(), ["Desk"]);
  assert.equal(await box(grace).inputValue(), "@Claude Code ");
  await box(grace).press("End");
  await grace.keyboard.type("Tidy the readme");
  await grace.keyboard.press("Enter");
  await grace.getByText("grace's Claude Code · Desk").waitFor();
  assert.equal(desk.heard().length, 1);
});
