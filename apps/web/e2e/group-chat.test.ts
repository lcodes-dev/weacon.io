import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import { machine } from "./core.ts";
import { waitFor } from "./harness.ts";
import { type Served, serve, signIn, signInAs } from "./server.ts";

// A page of its own browser context, signed in as `email`.
async function another(served: Served, email: string): Promise<Page> {
  const context = await served.page.context().browser()?.newContext();
  if (context === undefined) {
    throw new Error("no browser");
  }
  const page = await context.newPage();
  await served.admin("add-account", "--email", email);
  await page.goto(served.url);
  await signInAs(served, page, email);
  return page;
}

const box = (page: Page) => page.getByRole("combobox", { name: "Message" });
const hint = "Another person's agent is working in this chat, so the message wasn't sent. Try again once it's done.";

// Picks the person's own agent for the open chat, then calls it with `text`.
async function firstCall(page: Page, text: string): Promise<void> {
  await page.getByRole("button", { name: "Add your agent" }).click();
  const fields = page.getByRole("group", { name: "Your agent in this chat" });
  await fields.getByLabel("Repository").selectOption("demo");
  await fields.getByLabel("Agent").selectOption("claude");
  // The box opens on the agent's mention once it is picked.
  await page.waitForFunction(() => document.querySelector("textarea")?.value.startsWith("@Claude Code ") === true);
  await box(page).press("End");
  await page.keyboard.type(text);
  await page.keyboard.press("Enter");
}

test("two people talk in one chat, and each calls only their own agent, which reads the others' words as quotes", { timeout: 900_000 }, async (t) => {
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
  const laptop = await machine(served, workspace, "Laptop");
  t.after(() => laptop.close());
  await ada.getByRole("button", { name: "Back" }).click();

  const grace = await another(served, "grace@example.com");
  await grace.getByRole("list", { name: "Invitations" }).getByRole("button", { name: "Accept" }).click();
  await grace.getByRole("heading", { name: "Crew" }).waitFor();
  await grace.getByRole("button", { name: "Settings", exact: true }).click();
  const desk = await machine({ ...served, page: grace }, workspace, "Desk");
  t.after(() => desk.close());
  await grace.getByRole("button", { name: "Back" }).click();

  // Ada starts a chat with a plain message: no agent is asked for, and none starts.
  await ada.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "New chat" }).click();
  const form = ada.getByRole("region", { name: "New chat" });
  // With her machine picked the box opens on her agent's mention; writing over it leaves a plain message.
  await ada.waitForFunction(() => document.querySelector("textarea")?.value === "@Claude Code ");
  await box(form.page()).fill("Shall we fix the parser?");
  await form.getByRole("button", { name: "Send" }).click();
  await ada.getByRole("heading", { name: "Shall we fix the parser?" }).waitFor();
  await ada.locator(".row.prompt", { hasText: "Shall we fix the parser?" }).getByText("ada", { exact: true }).waitFor();

  // Grace answers in Ada's chat, under her own name.
  await grace.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "Shall we fix the parser?" }).click();
  await grace.getByText("Shall we fix the parser?").nth(1).waitFor();
  await box(grace).fill("Try the cache first");
  await grace.getByRole("button", { name: "Send" }).click();
  await ada.locator(".row.prompt", { hasText: "Try the cache first" }).getByText("grace", { exact: true }).waitFor();
  assert.deepEqual([laptop.heard(), desk.heard()], [[], []]);

  // Ada calls her agent, which reads what was said as quotes and answers under her name and machine.
  await firstCall(ada, "Run the parser tests");
  await ada.getByText("ada's Claude Code · Laptop").waitFor();
  await ada.getByRole("button", { name: "Allow" }).waitFor();
  const [told] = laptop.heard();
  assert.ok(told !== undefined && told.startsWith("@Claude Code Run the parser tests\n\n---\n"), told);
  assert.ok(told.includes("[[chat messages]]\nada wrote:\nShall we fix the parser?\n\ngrace wrote:\nTry the cache first\n[[end of chat messages]]"), told);

  // Grace sees whose answer it waits for, and can neither answer nor stop it, nor call her own meanwhile.
  await grace.getByText("Waiting for ada").waitFor();
  assert.equal(await grace.getByRole("button", { name: "Allow" }).count(), 0);
  assert.equal(await grace.getByRole("button", { name: "Stop" }).count(), 0);
  await firstCall(grace, "Tidy the readme");
  await grace.getByText(hint).waitFor();
  assert.equal(await box(grace).inputValue(), "@Claude Code Tidy the readme");
  assert.deepEqual(desk.heard(), []);
  // A plain message still goes while the agent works.
  await ada.getByRole("button", { name: "Allow" }).click();
  await ada.getByText("All 12 tests pass.").waitFor();
  await ada.getByText("Done", { exact: true }).waitFor();

  // Once Ada's agent is done, Grace's own takes her call on her machine, with Ada's agent's answer quoted.
  await grace.getByText("Done", { exact: true }).waitFor();
  await grace.getByRole("button", { name: "Send" }).click();
  await grace.getByText("grace's Claude Code · Desk").waitFor();
  await grace.getByText("Asks to use Write").waitFor();
  await grace.getByRole("button", { name: "Deny" }).waitFor();
  // Ada's agent's turn keeps its own label and its own answered prompt.
  assert.equal(await grace.getByText("ada's Claude Code · Laptop").count(), 1);
  assert.equal(await grace.getByText("Allowed Bash").count(), 1);
  await waitFor(() => desk.heard().length === 1);
  const [quoted] = desk.heard();
  assert.ok(quoted !== undefined && quoted.includes("ada's Claude Code on Laptop wrote:\nAll 12 tests pass."), quoted);
  assert.ok(quoted.includes("ada wrote:\n@Claude Code Run the parser tests"), quoted);
  assert.equal(laptop.heard().length, 1);
  await ada.getByText("Waiting for grace").waitFor();
  assert.equal(await ada.getByRole("button", { name: "Deny" }).count(), 0);
  await grace.getByRole("button", { name: "Deny" }).click();
  await grace.getByText("Done", { exact: true }).nth(1).waitFor();
  await ada.getByText("grace's Claude Code · Desk").waitFor();
  assert.equal(await ada.getByText("ada's Claude Code · Laptop").count(), 1);
  assert.equal(await ada.getByText("Allowed", { exact: false }).count(), 1);
});
