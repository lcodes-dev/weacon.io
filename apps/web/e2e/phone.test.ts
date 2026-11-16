import assert from "node:assert/strict";
import { test } from "node:test";

import type { Locator, Page } from "playwright-core";

import type { ChatAction, ChatList, DocRecord, Envelope, ProposalList, ProposalOutcome, ProposalRecord, WorkspaceList } from "../src/protocol.ts";
import { machine } from "./core.ts";
import { serveFeed } from "./feed.ts";
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
  // Rows reach their height after they arrive, and only then does the chat scroll to its end. If it never does, the checks below say where the prompt is.
  await page
    .waitForFunction(
      (prompt) => {
        const [at, box] = [prompt?.getBoundingClientRect(), document.querySelector(".composer")?.getBoundingClientRect()];
        return at !== undefined && box !== undefined && at.top >= 0 && at.bottom <= box.top;
      },
      await card.elementHandle(),
      { timeout: 10_000 },
    )
    .catch(() => undefined);

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
  await thumbSized(page, page.getByRole("button", { name: "Switch to light" }), "the theme switch");
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
  // The box opens on the agent's mention, which the message follows.
  await page.waitForFunction(() => document.querySelector("textarea")?.value === "@Claude Code ");
  await form.getByRole("combobox", { name: "Message" }).press("End");
  await page.keyboard.type("Run the parser tests");
  await fits(page, "New chat with a call");
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

// A phone of its own, signed in as `email`, whose account an invitation made.
async function anotherPhone(served: Served, email: string): Promise<Page> {
  const context = await served.page.context().browser()?.newContext(PHONE);
  if (context === undefined) {
    throw new Error("no browser");
  }
  const page = await context.newPage();
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

const ONE_LINE = `cargo test --locked -p parser -- ${"--skip slow_case_with_a_long_name ".repeat(12)}--nocapture`;
const MANY_LINES = Array.from({ length: 30 }, (_, at) => `echo "step ${at + 1} of the pantry sort"`).join(" && \\\n");
const WRITE = `notes/${"deeply-nested-folder/".repeat(8)}pantry.md\n${"A first line of the new text that runs on well past the edge of a phone. ".repeat(4)}\n${"More of the new text.\n".repeat(160)}`.slice(0, 4096);
const WANTED: Record<string, { tool: string; text: string }> = { "c-1": { tool: "Bash", text: ONE_LINE }, "c-2": { tool: "Bash", text: MANY_LINES }, "c-3": { tool: "Write", text: WRITE } };

// The member whose agent asks in `waitingChat`: the signed-in person, once known.
let asker = "";

// A chat's snapshot waiting on a prompt whose tool wants what `WANTED` holds for the chat; every command is taken.
function waitingChat(asked: Envelope): Envelope[] {
  if (asked.type === "chat_command") {
    return [{ id: asked.id, type: "receipt", request: asked.request, state: "accepted" }];
  }
  const wanted = asked.type === "chat_query" && asked.query.kind === "snapshot" ? WANTED[asked.chat] : undefined;
  if (asked.type !== "chat_query" || wanted === undefined) {
    return [];
  }
  const agent = { kind: "claude" as const, owner: { kind: "member" as const, member: asker }, machine: "laptop" };
  const asks = { event: "permission_asked" as const, run: "r-1", prompt: 1, tool: wanted.tool, input: {}, wants: { text: wanted.text, cut: asked.chat === "c-3" } };
  const events = [
    { seq: 1, event: { kind: "prompt" as const, request: "q-1", text: "Sort the pantry." } },
    { seq: 2, event: { kind: "run" as const, update: { run: "r-1", event: asks } } },
  ];
  return [{ id: asked.id, type: "chat_answer", workspace: asked.workspace, chat: asked.chat, answer: { kind: "snapshot", seq: 2, runs: [{ run: "r-1", state: "waiting", agent }], waiting: [2], events } }];
}

test("on a phone a prompt with a long command or a whole file keeps its text in its box and its answers in reach", { timeout: 600_000 }, async (t) => {
  const served = await serve(PHONE);
  t.after(() => served.close());
  const { page } = served;
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands"], answer: waitingChat });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).waitFor();
  const member = await page.evaluate(async () => ((await (await fetch("/api/workspaces")).json()) as WorkspaceList).workspaces[0]?.member ?? "");
  asker = member;
  const legs = [{ member, machine: "laptop", machine_id: "mc-1", agent: "claude" as const, detached: false }];
  const chats = Object.keys(WANTED).map((id) => ({ id, title: `Pantry ${id}`, owner: member, legs, state: "open" as const, updated_at: 1 }));
  const list: ChatList = { chats };
  await page.route("**/api/workspaces/*/chats", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(list) }));
  await page.getByRole("button", { name: "Personal" }).tap();
  const nav = page.getByRole("navigation", { name: "Chats" });

  for (const [id, { tool, text }] of Object.entries(WANTED)) {
    await nav.getByRole("button", { name: `Pantry ${id}` }).tap();
    const card = page.getByRole("group", { name: `Use ${tool}?` });
    const wants = card.locator(".wants");
    // By its text, since the card of the chat left a moment ago may name the same tool.
    await wants.filter({ hasText: text.slice(0, 40) }).waitFor();
    assert.equal(await wants.textContent(), text);
    for (const width of [360, 390]) {
      await page.setViewportSize({ width, height: 844 });
      // The text wraps, and what is too tall scrolls inside its own box, so the card and its answers stay whole on screen.
      assert.equal(await wants.evaluate((part) => part.scrollWidth > part.clientWidth), false, `${id}'s text is cut off sideways at ${width}px`);
      const whole = await shown(page, card, `${id}'s prompt`);
      const composer = await shown(page, page.locator(".composer"), "the message box");
      assert.ok(whole.y + whole.height <= composer.y, `${id}'s prompt is under the message box`);
      const allow = await thumbSized(page, card.getByRole("button", { name: "Allow" }), "Allow");
      const deny = await thumbSized(page, card.getByRole("button", { name: "Deny" }), "Deny");
      assert.ok(deny.x - (allow.x + allow.width) >= 8, "Allow and Deny touch");
    }
    await fits(page, `${id}'s prompt`);
    await page.getByRole("button", { name: "Back to chats" }).tap();
  }

  await nav.getByRole("button", { name: "Pantry c-3" }).tap();
  const card = page.getByRole("group", { name: "Use Write?" });
  assert.ok(await card.locator(".wants").evaluate((part) => part.scrollHeight > part.clientHeight), "the whole file's text fits, so nothing is checked");
  await card.getByText("The rest is too long to show.").waitFor();
  await card.getByRole("button", { name: "Deny" }).tap();
  await waitFor(() => sent.some((envelope) => envelope.type === "chat_command" && envelope.action.kind === "permission"));
  assert.deepEqual(
    sent.flatMap((envelope) => (envelope.type === "chat_command" && envelope.action.kind === "permission" ? [[envelope.chat, envelope.action.answer]] : [])),
    [["c-3", { run: "r-1", prompt: 1, allow: false }]],
  );
});

test("on a phone two people's chat fits: their names, the agent's fields, whose answer it waits for, and a worktree that can't go yet", { timeout: 900_000 }, async (t) => {
  const served = await serve(PHONE);
  t.after(() => served.close());
  const ada = served.page;
  const box = (page: Page) => page.getByRole("combobox", { name: "Message" });
  const chats = (page: Page) => page.getByRole("navigation", { name: "Chats" });
  await signIn(served);
  await ada.getByLabel("Name").fill("Crew");
  await ada.getByRole("button", { name: "Create" }).tap();
  await chats(ada).waitFor();
  const workspace = new URL(ada.url()).searchParams.get("workspace");
  assert.ok(workspace !== null);
  await ada.getByRole("button", { name: "Settings", exact: true }).tap();
  const members = ada.getByRole("region", { name: "Members" });
  await members.getByLabel("Email").fill("grace@example.com");
  await members.getByRole("button", { name: "Invite" }).tap();
  await members.getByText("Invited grace@example.com.", { exact: false }).waitFor();
  const laptop = await machine(served, workspace, "Laptop");
  t.after(() => laptop.close());
  await ada.getByRole("button", { name: "Back", exact: true }).tap();
  const grace = await anotherPhone(served, "grace@example.com");
  await grace.getByRole("list", { name: "Invitations" }).getByRole("button", { name: "Accept" }).tap();
  await chats(grace).waitFor();

  // Ada starts with a plain message, written over the mention the box opens on.
  await chats(ada).getByRole("button", { name: "New chat" }).tap();
  const form = ada.getByRole("region", { name: "New chat" });
  await ada.waitForFunction(() => document.querySelector("textarea")?.value === "@Claude Code ");
  await shown(ada, form.getByRole("group", { name: "Your agent in this chat" }), "the agent's fields");
  await fits(ada, "New chat with the agent's fields");
  await box(ada).fill("Shall we fix the parser?");
  await form.getByRole("button", { name: "Send" }).tap();
  await ada.getByRole("heading", { name: "Shall we fix the parser?" }).waitFor();

  // Grace, who has no machine, answers under her own name.
  await chats(grace).getByRole("button", { name: "Shall we fix the parser?" }).tap();
  await grace.locator(".row.prompt", { hasText: "Shall we fix the parser?" }).getByText("ada", { exact: true }).waitFor();
  await thumbSized(grace, grace.getByRole("button", { name: "Add your agent" }), "Add your agent");
  await box(grace).fill("Try the cache first, and tell me how long the whole suite takes once it is warm");
  await grace.getByRole("button", { name: "Send" }).tap();
  await ada.locator(".row.prompt", { hasText: "Try the cache first" }).getByText("grace", { exact: true }).waitFor();
  await fits(ada, "a chat two people write in");

  // Ada adds her agent in the chat and calls it.
  await ada.getByRole("button", { name: "Add your agent" }).tap();
  const fields = ada.getByRole("group", { name: "Your agent in this chat" });
  await fields.getByLabel("Repository").selectOption("demo");
  await fields.getByLabel("Agent").selectOption("claude");
  await ada.waitForFunction(() => document.querySelector("textarea")?.value.startsWith("@Claude Code ") === true);
  await fits(ada, "a chat with the agent's fields");
  await shown(ada, ada.locator(".composer"), "the message box");
  await box(ada).press("End");
  await ada.keyboard.type("Run the parser tests");
  await thumbSized(ada, ada.getByRole("button", { name: "Send" }), "Send");
  await ada.getByRole("button", { name: "Send" }).tap();
  await ada.getByText("ada's Claude Code · Laptop").waitFor();
  await thumbSized(ada, ada.getByRole("button", { name: "Allow" }), "Allow");
  await fits(ada, "a chat waiting on its owner");

  // Grace sees whose answer it waits for, whole on her screen, and writes into the turn.
  await shown(grace, grace.getByText("Waiting for ada"), "whose answer it waits for");
  assert.equal(await grace.getByRole("button", { name: "Allow" }).count(), 0);
  await fits(grace, "a chat waiting on someone else");
  await box(grace).fill("Any luck?");
  await grace.getByRole("button", { name: "Send" }).tap();
  await ada.locator(".row.prompt", { hasText: "Any luck?" }).getByText("grace", { exact: true }).waitFor();

  // While the agent works its worktree stays, and the bar says why.
  await ada.getByRole("button", { name: "Changes" }).tap();
  const bar = ada.getByRole("region", { name: "Worktree" });
  await bar.getByRole("button", { name: "Remove" }).tap();
  const confirm = bar.getByRole("group", { name: "Remove the worktree" });
  await thumbSized(ada, confirm.getByRole("button", { name: "Remove" }), "Remove");
  await fits(ada, "removing a worktree");
  await confirm.getByRole("button", { name: "Remove" }).tap();
  await shown(ada, bar.getByText("Stop the agent first, then remove the worktree."), "why the worktree stays");
  assert.equal(await bar.getByRole("button", { name: "Restore" }).count(), 0);
  await fits(ada, "a worktree that can't go yet");
  await ada.getByRole("button", { name: "Back", exact: true }).tap();

  // The turn carries on after Grace's message, in two parts under the agent's label.
  await ada.getByRole("button", { name: "Allow" }).tap();
  await ada.getByText("All 12 tests pass.").waitFor();
  await ada.getByText("Done", { exact: true }).waitFor();
  await grace.getByText("All 12 tests pass.").waitFor();
  await fits(ada, "a finished turn someone wrote into");
  await fits(grace, "a finished turn, for the one who wrote into it");

  // Grace has no machine to add an agent from, and is told what to do.
  await grace.getByRole("button", { name: "Add your agent" }).tap();
  await grace.getByRole("button", { name: "Open Settings" }).waitFor();
  const note = await shown(grace, grace.getByText("Pair a machine in Settings to call your own agent."), "what is missing");
  assert.ok(note.x >= 8, "what is missing touches the screen's edge");
  await shown(grace, grace.locator(".composer"), "the message box");
  await fits(grace, "a chat with no machine to add an agent from");
});
