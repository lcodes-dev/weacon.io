import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import type { DocRecord, ProposalList, ProposalOutcome, ProposalRecord, TaskList, WorkspaceList } from "../src/protocol.ts";
import { type Served, serve, signIn } from "./server.ts";

/**
 * Asks the server from the page, as another tab of the same person would, and gives its answer;
 * from the page, since the session's cookie is kept for the browser's secure origins only.
 */
async function ask<T>(page: Page, method: string, path: string, body?: unknown): Promise<T> {
  return page.evaluate(
    async ({ method, path, body }) => {
      const init: RequestInit = body === undefined ? { method } : { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
      return (await fetch(path, init)).json() as Promise<T>;
    },
    { method, path, body },
  );
}

/** Signs in, opens the Personal workspace's `section`, and gives the workspace's path and the person's member. */
async function open(served: Served, section: string): Promise<{ base: string; member: string }> {
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name: section }).click();
  await page.getByRole("heading", { name: section }).waitFor();
  const { workspaces } = await ask<WorkspaceList>(page, "GET", "/api/workspaces");
  const personal = workspaces.find(({ name }) => name === "Personal");
  assert.ok(personal?.member !== undefined);
  return { base: `/api/workspaces/${personal.id}`, member: personal.member };
}

test("a member adds tasks, moves one along and retitles one, and an edit made meanwhile is reported and kept", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { base } = await open(served, "Tasks");
  await page.getByText("No tasks yet.").waitFor();

  for (const title of ["Buy flour", "Bake bread"]) {
    await page.getByLabel("New task").fill(` ${title} `);
    await page.getByRole("button", { name: "Add" }).click();
    await page.getByRole("button", { name: `Edit ${title}` }).waitFor();
  }
  assert.equal(await page.getByLabel("New task").inputValue(), "");
  await page.getByLabel("Status of Buy flour").selectOption("done");
  await page.waitForFunction(() => document.querySelector(".title.done")?.textContent === "Buy flour");

  await page.getByRole("button", { name: "Edit Buy flour" }).click();
  await page.getByLabel("Title").fill("Buy rye flour");
  const { tasks } = await ask<TaskList>(page, "GET", `${base}/tasks`);
  const flour = tasks.find(({ title }) => title === "Buy flour");
  assert.ok(flour !== undefined);
  await ask(page, "POST", `${base}/tasks/${flour.id}`, { version: flour.version, edit: { kind: "title", title: "Buy spelt flour" } });
  await page.getByRole("button", { name: "Save" }).click();
  await page.getByText("This task changed since you read it. Check it, then try again.").waitFor();
  await page.getByText("It now reads “Buy spelt flour”. Save to put yours in its place.").waitFor();
  assert.equal(await page.getByLabel("Title").inputValue(), "Buy rye flour");
  await page.getByRole("button", { name: "Save" }).click();
  await page.getByRole("button", { name: "Edit Buy rye flour" }).waitFor();

  await page.reload();
  const list = page.getByRole("list", { name: "Tasks" });
  await list.getByRole("button", { name: "Edit Buy rye flour" }).waitFor();
  assert.deepEqual(await list.locator(".title").allInnerTexts(), ["Buy rye flour", "Bake bread"]);
  assert.equal(await page.getByLabel("Status of Buy rye flour").inputValue(), "done");
  assert.match(page.url(), /view=tasks/);
});

test("a member writes a doc, reads who revised it, and keeps their edit when someone saved meanwhile", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { base } = await open(served, "Docs");
  await page.getByText("No docs yet.").waitFor();

  await page.getByRole("button", { name: "New doc" }).click();
  await page.getByLabel("Title").fill("Recipes ");
  await page.getByLabel("Text", { exact: true }).fill("# Bread\n\nFlour and **water**.\n");
  await page.getByRole("button", { name: "Save" }).click();
  await page.getByRole("heading", { name: "Recipes" }).waitFor();
  await page.getByText("Revision 1 by ada").waitFor();
  assert.equal(await page.locator(".docs strong").innerText(), "water");
  const doc = /doc=([A-Za-z0-9_-]+)/.exec(page.url())?.[1] ?? "";

  await page.getByRole("button", { name: "Edit" }).click();
  await page.getByLabel("Text", { exact: true }).fill("# Bread\n\nFlour, water and salt.\n");
  const read = await ask<DocRecord>(page, "GET", `${base}/docs/${doc}`);
  await ask(page, "POST", `${base}/docs/${doc}`, { version: read.version, content: { title: "Recipes", body: "# Bread\n\nFlour, water and yeast.\n" } });
  await page.getByRole("button", { name: "Save" }).click();
  await page.getByText("Someone saved this doc since you read it.").waitFor();
  const compared = page.getByRole("group", { name: "Their text and yours" });
  await compared.locator("ins").getByText("Flour, water and salt.").waitFor();
  assert.equal(await compared.locator("del").innerText(), "Flour, water and yeast.");
  assert.equal(await page.getByLabel("Text", { exact: true }).inputValue(), "# Bread\n\nFlour, water and salt.\n");
  await page.getByRole("button", { name: "Save mine over theirs" }).click();
  await page.getByText("Revision 3 by ada").waitFor();
  await page.getByText("Flour, water and salt.").waitFor();

  await page.getByRole("button", { name: "All docs" }).click();
  await page.getByRole("list", { name: "Docs" }).getByRole("button", { name: "Recipes" }).click();
  await page.getByText("Revision 3 by ada").waitFor();
  await page.goBack();
  const docs = page.getByRole("list", { name: "Docs" });

  // Moving from one doc straight to another leaves the first one's edit behind.
  const chores = await ask<DocRecord>(page, "POST", `${base}/docs`, { title: "Chores", body: "Sweep.\n" });
  await docs.getByRole("button", { name: "Chores" }).click();
  await page.getByRole("heading", { name: "Chores" }).waitFor();
  await page.getByRole("button", { name: "All docs" }).click();
  await docs.getByRole("button", { name: "Recipes" }).click();
  await page.getByRole("button", { name: "Edit" }).click();
  await page.getByLabel("Text", { exact: true }).fill("Not for the chores.");
  await page.evaluate(() => window.history.go(-2));
  await page.getByRole("heading", { name: "Chores" }).waitFor();
  await page.getByRole("button", { name: "Edit" }).waitFor();
  assert.equal(await page.getByLabel("Text", { exact: true }).count(), 0);
  assert.equal((await ask<DocRecord>(page, "GET", `${base}/docs/${chores.id}`)).content.body, "Sweep.\n");
});

test("an agent's proposals show with their changes and who proposed them, and any member accepts or rejects them", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { base, member } = await open(served, "Proposals");
  await page.getByText(/^No open proposals/).waitFor();
  const doc = await ask<DocRecord>(page, "POST", `${base}/docs`, { title: "Recipes", body: "# Bread\nFlour.\nWater.\n" });
  const author = { kind: "agent", owner: member, agent: { kind: "claude", owner: { kind: "member", member }, machine: "laptop" } } as const;
  const edit: ProposalRecord = {
    id: "p-1",
    about: { kind: "doc", doc: doc.id },
    author,
    state: "open",
    at: 1_800_000_000,
    change: { kind: "doc_edit", doc: doc.id, base: doc.version, content: { title: "Recipes", body: "# Bread\nFlour.\nSalt.\n" } },
  };
  const task: ProposalRecord = { id: "p-2", about: { kind: "new_task", task: "t-1" }, author, state: "open", at: 1_800_000_001, change: { kind: "new_task", title: "Buy salt" } };
  // The server takes proposals only from machines, so they are served here as it would serve them.
  let decided: ProposalRecord[] = [];
  // A decided proposal no longer holds its change.
  const close = ({ change: _, ...open }: ProposalRecord, state: object) => ({ ...open, ...state }) as ProposalRecord;
  const decisions: unknown[] = [];
  await page.route("**/api/workspaces/*/proposals", (route) => {
    const proposals = [task, edit].filter(({ id }) => !decided.some((done) => done.id === id));
    const list: ProposalList = { proposals: [...proposals, ...decided] };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(list) });
  });
  await page.route("**/api/workspaces/*/proposals/*", (route) => {
    const body = route.request().postDataJSON() as { verdict: string };
    decisions.push([route.request().url().split("/").at(-1), body]);
    const accepted = decisions.length > 1;
    const outcome: ProposalOutcome = body.verdict === "reject" ? { outcome: "rejected" } : accepted ? { outcome: "accepted", version: 2 } : { outcome: "doc_conflict", hunks: [{ kind: "agreed", text: "# Bread\nFlour.\n" }, { kind: "conflict", base: "Water.\n", current: "Milk.\n", proposed: "Salt.\n" }], title: false, anchors: false };
    if (body.verdict === "reject") {
      decided = [...decided, close(task, { state: "rejected", by: member })];
    } else if (accepted) {
      decided = [...decided, close(edit, { state: "accepted", by: member, version: 2 })];
    }
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(outcome) });
  });
  await page.getByRole("button", { name: "Tasks" }).click();
  await page.getByRole("button", { name: "Proposals" }).click();

  const waiting = page.getByRole("list", { name: "Open proposals" });
  const card = waiting.getByRole("article", { name: "Edit of the doc “Recipes”" });
  await card.getByText("Proposed by ada's Claude Code · laptop").waitFor();
  const changes = card.getByRole("group", { name: "Its changes" });
  await changes.locator("ins").waitFor();
  assert.equal(await changes.locator("ins").innerText(), "Salt.");
  assert.equal(await changes.locator("del").innerText(), "Water.");
  await waiting.getByRole("article", { name: "New task “Buy salt”" }).waitFor();

  await card.getByRole("button", { name: "Accept" }).click();
  await card.getByText(/^The doc changed where this edit changes it too/).waitFor();
  assert.equal(await card.getByRole("group", { name: "The doc now and this edit" }).locator("del").innerText(), "Milk.");
  await card.getByRole("button", { name: "Accept" }).click();
  const done = page.getByRole("list", { name: "Decided proposals" });
  await done.getByText("Proposed by ada's Claude Code · laptop · Accepted by ada").waitFor();
  await waiting.getByRole("article", { name: "New task “Buy salt”" }).getByRole("button", { name: "Reject" }).click();
  await done.getByText("Proposed by ada's Claude Code · laptop · Rejected by ada").waitFor();
  await page.getByText(/^No open proposals/).waitFor();
  assert.deepEqual(decisions, [
    ["p-1", { verdict: "accept" }],
    ["p-1", { verdict: "accept" }],
    ["p-2", { verdict: "reject" }],
  ]);
});
