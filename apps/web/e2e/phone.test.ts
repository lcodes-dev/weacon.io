import assert from "node:assert/strict";
import { test } from "node:test";

import type { Locator, Page } from "playwright-core";

import type { ChatAction, DocRecord, Envelope, ProposalList, ProposalOutcome, ProposalRecord, WorkspaceList } from "../src/protocol.ts";
import { machine } from "./core.ts";
import { FAKE_CHAT, PHONE, fakeServer, start, waitFor } from "./harness.ts";
import { type Served, serve, signIn, signInAs } from "./server.ts";

const CHAT = { query: FAKE_CHAT, device: PHONE };
const THUMB = 44;

// Fails if the page scrolls at all, or a part of it that scrolls down also scrolls sideways, at either phone width.
async function fits(page: Page, where: string): Promise<void> {
  const height = page.viewportSize()?.height ?? 0;
  for (const width of [360, 390]) {
    await page.setViewportSize({ width, height });
    const wide = await page.evaluate(() =>
      [document.documentElement, ...document.querySelectorAll(".page, .side, .new-chat")]
        .filter((part) => part.scrollWidth > part.clientWidth)
        .map((part) => `${part.tagName.toLowerCase()}.${part.className}`),
    );
    assert.deepEqual(wide, [], `${where} scrolls sideways at ${width}px`);
    assert.ok(await page.evaluate(() => document.documentElement.scrollHeight <= document.documentElement.clientHeight), `${where} scrolls the whole page at ${width}px`);
  }
}

// Where `control` is, which must be whole inside the screen.
async function shown(page: Page, control: Locator, name: string) {
  const [box, view] = [await control.boundingBox(), page.viewportSize()];
  assert.ok(box !== null && view !== null, `${name} isn't shown`);
  const inside = box.x >= 0 && box.y >= 0 && box.x + box.width <= view.width && box.y + box.height <= view.height;
  assert.ok(inside, `${name} is at ${JSON.stringify(box)}, outside the screen`);
  return box;
}

// Where `button` is, which must be whole inside the screen and tall enough for a thumb.
async function thumbSized(page: Page, button: Locator, name: string) {
  const box = await shown(page, button, name);
  assert.ok(box.height >= THUMB, `${name} is ${box.height}px tall`);
  return box;
}

function actions(received: unknown[]): ChatAction[] {
  return (received as Envelope[]).flatMap((sent) => (sent.type === "chat_command" ? [sent.action] : []));
}

test("on a phone a waiting prompt shows whole above the message box, and a tap on a thumb-sized answer sends it", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  const card = page.getByRole("group", { name: "Use Bash?" });
  await card.getByText("Asks to use Bash").waitFor();

  const whole = await shown(page, card, "the prompt");
  const composer = await shown(page, page.locator(".composer"), "the message box");
  assert.ok(whole.y + whole.height <= composer.y, "the prompt is under the message box");
  assert.equal(await card.evaluate((part) => part.scrollWidth > part.clientWidth), false);
  const allow = await thumbSized(page, card.getByRole("button", { name: "Allow" }), "Allow");
  const deny = await thumbSized(page, card.getByRole("button", { name: "Deny" }), "Deny");
  assert.ok(deny.x - (allow.x + allow.width) >= 8, "Allow and Deny touch");
  assert.ok(allow.width >= 100 && deny.width >= 100, "the answers are narrow");
  await fits(page, "the chat");

  await card.getByRole("button", { name: "Allow" }).tap();
  await page.getByText("Allowed Bash").waitFor();
  assert.deepEqual(
    actions(harness.received()).filter((action) => action.kind === "permission"),
    [{ kind: "permission", answer: { run: "r-1", prompt: 1, allow: true } }],
  );
});

test("on a phone the message box stays on screen with thumb-sized Send and Stop, and a diff's lines scroll inside it", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 1), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByText("Asks to use Bash").waitFor();
  const box = page.getByRole("combobox", { name: "Message" });
  const send = page.getByRole("button", { name: "Send" });
  const stop = page.getByRole("button", { name: "Stop" });

  const composer = await shown(page, page.locator(".composer"), "the message box");
  assert.equal(composer.y + composer.height, page.viewportSize()?.height);
  await thumbSized(page, stop, "Stop");
  await box.fill("A word too long to fit on one line of a phone: " + "pneumonoultramicroscopic".repeat(6));
  await thumbSized(page, send, "Send");
  await shown(page, box, "the message");
  await send.tap();
  await page.locator(".row.prompt").filter({ hasText: "A word too long" }).waitFor();
  await fits(page, "the chat");
  await shown(page, page.locator(".composer"), "the message box");

  await stop.tap();
  await waitFor(() => actions(harness.received()).some((action) => action.kind === "interrupt"));

  await page.getByRole("button", { name: "Changes" }).tap();
  const lines = page.locator(".diff .lines").first();
  await lines.waitFor();
  assert.ok(await lines.evaluate((part) => part.scrollWidth > part.clientWidth), "the first file's lines all fit, so nothing is checked");
  await fits(page, "the changes");
  await lines.locator(".line").nth(3).tap({ position: { x: 8, y: 10 } });
  // The comment stays put while the lines scroll sideways under it.
  await lines.evaluate((part) => part.scrollTo(part.scrollWidth, 0));
  await shown(page, page.locator(".diff .comment"), "a comment");
  await thumbSized(page, page.locator(".diff .comment").getByRole("button", { name: "Cancel" }), "a comment's Cancel");
  await fits(page, "a comment on the changes");
});

test("on a phone the chats show first, a chat fills the screen, and every page fits", { timeout: 900_000 }, async (t) => {
  const served = await serve(PHONE);
  t.after(() => served.close());
  const { page } = served;
  const nav = page.getByRole("navigation", { name: "Chats" });
  const sections = page.getByRole("navigation", { name: "Sections" });
  const toChats = page.getByRole("button", { name: "Back to chats" });
  await page.getByLabel("Email").waitFor();
  await fits(page, "sign-in");
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).waitFor();
  await fits(page, "the workspaces");

  // The list alone: the workspace's own heading belongs to what opens from it.
  await page.getByRole("button", { name: "Personal" }).tap();
  await nav.waitFor();
  assert.equal(await page.getByRole("heading", { name: "Personal" }).count(), 0);
  assert.equal(await toChats.count(), 0);
  await thumbSized(page, page.getByRole("button", { name: "Workspace: Personal" }), "the workspace selector");
  await fits(page, "the chat list");
  const workspace = new URL(page.url()).searchParams.get("workspace");
  assert.ok(workspace !== null);

  await page.getByRole("button", { name: "Settings", exact: true }).tap();
  const studio = await machine(served, workspace, "Studio");
  t.after(() => studio.close());
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });
  // Settings opened afresh, since a machine just paired may open its own by itself.
  await page.getByRole("button", { name: "Back", exact: true }).tap();
  await page.getByRole("button", { name: "Settings", exact: true }).tap();
  await row.getByRole("button", { name: "Studio settings" }).tap();
  const settings = row.getByRole("region", { name: "Studio settings" });
  await settings.getByRole("list", { name: "Sign-ins" }).waitFor();
  await fits(page, "a machine's settings");
  await settings.getByRole("button", { name: "Sign in again" }).tap();
  await settings.getByRole("button", { name: "Close the sign-in" }).waitFor();
  await fits(page, "the sign-in terminal");
  await page.getByRole("button", { name: "Back", exact: true }).tap();
  // The machines were listed before the core paired.
  await nav.getByRole("button", { name: "Check again" }).tap();

  // New chat fills the screen, and the one control back shows the list again.
  await nav.getByRole("button", { name: "New chat" }).tap();
  const form = page.getByRole("region", { name: "New chat" });
  await form.getByLabel("Repository").selectOption("demo");
  await form.getByLabel("Agent").selectOption("claude");
  assert.equal(await nav.count(), 0);
  await fits(page, "New chat");
  await thumbSized(page, toChats, "Back to chats");
  await toChats.tap();
  await nav.waitFor();
  assert.equal(await form.count(), 0);
  await nav.getByRole("button", { name: "New chat" }).tap();
  await form.getByLabel("Repository").selectOption("demo");
  await form.getByLabel("Agent").selectOption("claude");
  await form.getByRole("combobox", { name: "Message" }).fill("Run the parser tests");
  await thumbSized(page, form.getByRole("button", { name: "Send" }), "Send");
  await form.getByRole("button", { name: "Send" }).tap();

  // The chat alone, with its prompt answered by a tap.
  await page.getByRole("heading", { name: "Run the parser tests" }).waitFor();
  assert.equal(await nav.count(), 0);
  const allow = page.getByRole("button", { name: "Allow" });
  await thumbSized(page, allow, "Allow");
  await fits(page, "a chat");
  await allow.tap();
  await page.getByText("Done", { exact: true }).waitFor();
  assert.equal(await page.getByText("All 12 tests pass.").count(), 1);
  await shown(page, page.locator(".composer"), "the message box");
  await page.getByRole("button", { name: "Changes" }).tap();
  await page.getByRole("region", { name: "Worktree" }).getByRole("button", { name: "Open terminal" }).waitFor();
  await fits(page, "a chat's worktree");
  await page.getByRole("button", { name: "Back", exact: true }).tap();

  // Back to the list, into the chat from it, and back again with the browser's own Back.
  await toChats.tap();
  await nav.getByText("Run the parser tests").waitFor();
  assert.equal(await page.getByRole("heading", { name: "Run the parser tests" }).count(), 0);
  assert.equal(new URL(page.url()).searchParams.get("chat"), null);
  await nav.getByText("Run the parser tests").tap();
  await page.getByRole("heading", { name: "Run the parser tests" }).waitFor();
  assert.equal(await nav.count(), 0);
  await page.goBack();
  await nav.getByText("Run the parser tests").waitFor();

  // Search, which a phone has no key for, opens from the list.
  await page.getByRole("button", { name: "Search" }).tap();
  const search = page.getByRole("dialog", { name: "Search" });
  await search.getByRole("combobox", { name: "Search" }).fill("parser");
  await search.getByRole("option").first().waitFor();
  await shown(page, search, "the search");
  await fits(page, "the search");
  await search.getByRole("option").first().tap();
  await page.getByRole("heading", { name: "Run the parser tests" }).waitFor();
  await toChats.tap();

  await sections.getByRole("button", { name: "Tasks" }).tap();
  await page.getByRole("heading", { name: "Tasks" }).waitFor();
  assert.equal(await sections.count(), 0);
  const long = "Bake a loaf with seeds, rye and spelt, and a crust that takes the whole of the afternoon";
  for (const title of ["Buy flour", long]) {
    await page.getByLabel("New task").fill(title);
    await page.getByRole("button", { name: "Add" }).tap();
    await page.getByRole("button", { name: `Edit ${title}` }).waitFor();
  }
  await fits(page, "the tasks");
  await page.getByRole("button", { name: `Edit ${long}` }).tap();
  await thumbSized(page, page.getByRole("button", { name: "Save" }), "a task's Save");
  await shown(page, page.getByRole("button", { name: "Cancel" }), "a task's Cancel");
  await fits(page, "a task");
  await toChats.tap();

  await sections.getByRole("button", { name: "Docs" }).tap();
  await page.getByRole("button", { name: "New doc" }).tap();
  await page.getByLabel("Title").fill("Recipes");
  await page.getByLabel("Text", { exact: true }).fill("# Bread\n\n```\nlet dough = flour.mix(water).knead(minutes(10)).rest(hours(2)).shape().bake(celsius(230));\n```\n" + "Then wait.\n\n".repeat(40));
  await fits(page, "a doc being written");
  await page.getByRole("button", { name: "Save" }).tap();
  await page.getByText("Revision 1 by ada").waitFor();
  // The long line of code scrolls inside its own block.
  assert.ok(await page.locator(".docs pre code").evaluate((code) => code.scrollWidth > code.clientWidth));
  // A doc longer than the screen scrolls under the heading, which stays.
  await fits(page, "a doc");
  await shown(page, toChats, "Back to chats");
  await page.getByRole("button", { name: "All docs" }).tap();
  await page.getByRole("list", { name: "Docs" }).getByRole("button", { name: "Recipes" }).waitFor();
  await fits(page, "the docs");
  await toChats.tap();
  await nav.getByText("Run the parser tests").waitFor();
});

// Asks the server from the page, as another tab of the same person would.
async function ask<T>(page: Page, method: string, path: string, body?: unknown): Promise<T> {
  return page.evaluate(
    async ({ method, path, body }) => {
      const init: RequestInit = body === undefined ? { method } : { method, headers: { "content-type": "application/json" }, body: JSON.stringify(body) };
      return (await fetch(path, init)).json() as Promise<T>;
    },
    { method, path, body },
  );
}

// A phone of its own, signed in as `email`.
async function anotherPhone(served: Served, email: string): Promise<Page> {
  const context = await served.page.context().browser()?.newContext(PHONE);
  if (context === undefined) {
    throw new Error("no browser");
  }
  const page = await context.newPage();
  await served.admin("add-account", "--email", email);
  await page.goto(served.url);
  await signInAs(served, page, email);
  return page;
}

test("on a phone members, an invitation and proposals fit, and their answers are thumb-sized", { timeout: 600_000 }, async (t) => {
  const served = await serve(PHONE);
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).tap();
  await page.getByRole("navigation", { name: "Chats" }).waitFor();

  await page.getByRole("button", { name: "Settings", exact: true }).tap();
  const members = page.getByRole("region", { name: "Members" });
  await members.getByLabel("Email").fill("bo@example.com");
  await thumbSized(page, members.getByRole("button", { name: "Invite" }), "Invite");
  await members.getByRole("button", { name: "Invite" }).tap();
  await members.getByRole("list", { name: "Invitations" }).getByText("bo@example.com").waitFor();
  await fits(page, "Settings");

  const bo = await anotherPhone(served, "bo@example.com");
  const offer = bo.getByRole("list", { name: "Invitations" }).getByRole("listitem");
  await offer.getByText("Personal").waitFor();
  await fits(bo, "an invitation");
  await thumbSized(bo, offer.getByRole("button", { name: "Accept" }), "an invitation's Accept");
  await offer.getByRole("button", { name: "Accept" }).tap();
  await bo.getByRole("navigation", { name: "Chats" }).waitFor();
  await members.getByRole("list", { name: "Members" }).getByRole("listitem").filter({ hasText: "bo" }).waitFor();
  await fits(page, "the members");
  await page.getByRole("button", { name: "Back", exact: true }).tap();

  const { workspaces } = await ask<WorkspaceList>(page, "GET", "/api/workspaces");
  const personal = workspaces.find(({ name }) => name === "Personal");
  assert.ok(personal?.member !== undefined);
  const member = personal.member;
  const line = "Flour, water, salt and a spoonful of the starter kept from the loaf before, stirred until no dry flour is left.";
  const doc = await ask<DocRecord>(page, "POST", `/api/workspaces/${personal.id}/docs`, { title: "Recipes", body: `# Bread\n${line}\nWater.\n` });
  const author = { kind: "agent", owner: member, agent: { kind: "claude", owner: { kind: "member", member }, machine: "laptop" } } as const;
  const change = { kind: "doc_edit", doc: doc.id, base: doc.version, content: { title: "Recipes", body: `# Bread\n${line}\nSalt.\n` } } as const;
  const edit: ProposalRecord = { id: "p-1", about: { kind: "doc", doc: doc.id }, author, state: "open", at: 1_800_000_000, change };
  const task: ProposalRecord = { id: "p-2", about: { kind: "new_task", task: "t-1" }, author, state: "open", at: 1_800_000_001, change: { kind: "new_task", title: "Buy salt" } };
  // The server takes proposals only from machines, so they are served here as it would serve them.
  let open = [task, edit];
  const decisions: unknown[] = [];
  await page.route("**/api/workspaces/*/proposals", (route) => {
    const list: ProposalList = { proposals: open };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(list) });
  });
  await page.route("**/api/workspaces/*/proposals/*", (route) => {
    const id = route.request().url().split("/").at(-1);
    const { verdict } = route.request().postDataJSON() as { verdict: string };
    decisions.push([id, verdict]);
    open = open.filter((proposal) => proposal.id !== id);
    const outcome: ProposalOutcome = verdict === "reject" ? { outcome: "rejected" } : { outcome: "accepted", version: 2 };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(outcome) });
  });

  await page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name: "Proposals" }).tap();
  const waiting = page.getByRole("list", { name: "Open proposals" });
  const card = waiting.getByRole("article", { name: "Edit of the doc “Recipes”" });
  await card.getByRole("group", { name: "Its changes" }).locator("ins").waitFor();
  await fits(page, "the proposals");
  const accept = await thumbSized(page, card.getByRole("button", { name: "Accept" }), "Accept");
  const reject = await thumbSized(page, card.getByRole("button", { name: "Reject" }), "Reject");
  assert.ok(reject.x - (accept.x + accept.width) >= 8, "Accept and Reject touch");
  await card.getByRole("button", { name: "Accept" }).tap();
  await card.waitFor({ state: "detached" });
  await waiting.getByRole("article", { name: "New task “Buy salt”" }).getByRole("button", { name: "Reject" }).tap();
  await page.getByText(/^No open proposals/).waitFor();
  assert.deepEqual(decisions, [
    ["p-1", "accept"],
    ["p-2", "reject"],
  ]);
});
