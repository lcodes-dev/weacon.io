import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

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

const members = (page: Page) => page.getByRole("region", { name: "Members" });

async function invite(page: Page, email: string): Promise<void> {
  await members(page).getByLabel("Email").fill(email);
  await members(page).getByRole("button", { name: "Invite" }).click();
  await members(page).getByText(`Invited ${email}.`, { exact: false }).waitFor();
}

test("an owner invites someone, who signs in and joins, is made admin, then removed, and leaves at once", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByLabel("Name").fill("Crew");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("heading", { name: "Crew" }).waitFor();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await invite(page, "bo@example.com");
  await members(page).getByRole("list", { name: "Invitations" }).getByText("bo@example.com").waitFor();
  assert.deepEqual(served.notices(), [{ to: "bo@example.com", notice: { kind: "invited" } }]);

  // The server lets nobody sign up, but the invitation lets bo in, with no Personal workspace.
  const bo = await another(served, "bo@example.com");
  const offer = bo.getByRole("list", { name: "Invitations" }).getByRole("listitem");
  await offer.getByText("ada invited you as member").waitFor();
  assert.equal(await bo.getByRole("heading", { name: "Workspaces" }).count(), 0);
  await offer.getByRole("button", { name: "Accept" }).click();
  await bo.getByRole("heading", { name: "Crew" }).waitFor();
  await bo.getByRole("button", { name: "Workspace: Crew" }).click();
  assert.deepEqual(await bo.getByLabel("Workspaces").getByRole("listitem").allInnerTexts(), ["Crew"]);

  // The owner hears bo join without reloading.
  const row = members(page).getByRole("list", { name: "Members" }).getByRole("listitem").filter({ hasText: "bo" });
  await row.getByText("Member", { exact: true }).waitFor();
  await row.getByRole("button", { name: "Make admin" }).click();
  await row.getByText("Admin", { exact: true }).waitFor();
  await row.getByRole("button", { name: "Remove" }).click();
  await row.getByRole("button", { name: "Remove" }).click();
  await row.getByText("Past member").waitFor();

  await bo.getByText("You're no longer a member of Crew.").waitFor();
  assert.equal(await bo.getByRole("heading", { name: "Crew" }).count(), 0);
});

test("a hidden invitation stays pending to the workspace", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await invite(page, "cy@example.com");

  const cy = await another(served, "cy@example.com");
  await cy.getByRole("button", { name: "Hide" }).click();
  await cy.getByRole("heading", { name: "Invitations" }).waitFor({ state: "detached" });
  await cy.getByRole("heading", { name: "New workspace" }).waitFor();
  await cy.reload();
  await cy.getByRole("heading", { name: "New workspace" }).waitFor();
  assert.equal(await cy.getByRole("heading", { name: "Invitations" }).count(), 0);

  await page.reload();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await members(page).getByRole("list", { name: "Invitations" }).getByText("cy@example.com").waitFor();
});
