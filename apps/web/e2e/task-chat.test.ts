import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import type { ChatEntry, ChatList, ChatSummary, Envelope, MachineSettings } from "../src/protocol.ts";
import { serveFeed } from "./feed.ts";
import { type Served, serve, signIn } from "./server.ts";

const STUDIO = { id: "mc-1", name: "Studio", os: "macos" as const, paired_at: 1, connected: true };
const SETTINGS: MachineSettings = { access: "ask_first", team: false, sign_ins: [{ agent: "claude", state: "signed_in" }] };

// The workspace's chats are `chats`, its owner's, on Studio, the person's one machine.
async function standIn(page: Page, chats: () => Omit<ChatSummary, "owner" | "legs">[]): Promise<void> {
  await page.route("**/api/machines", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ machines: [STUDIO] }) }));
  await page.route("**/api/workspaces/*/chats", async (route) => {
    const member = await page.evaluate(async () => ((await (await fetch("/api/workspaces")).json()) as { workspaces: Array<{ member?: string }> }).workspaces[0]?.member ?? "");
    const legs = [{ member, machine: STUDIO.name, machine_id: STUDIO.id, agent: "claude" as const, detached: false }];
    const list: ChatList = { chats: chats().map((chat) => ({ ...chat, owner: member, legs })) };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(list) });
  });
}

async function openPersonal(served: Served): Promise<void> {
  await signIn(served);
  await served.page.getByRole("button", { name: "Personal" }).click();
  await served.page.getByRole("heading", { name: "Personal" }).waitFor();
}

const said = (seq: number): ChatEntry => ({ seq, event: { kind: "prompt", request: `q-${seq}`, text: seq === 5 ? "Where the compost goes" : `Message ${seq}` } });

test("a task starts a chat for it, and the chat links back to the task", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const made: { chat: string; task: string; text: string }[] = [];
  await standIn(page, () => made.map(({ chat, task, text }) => ({ id: chat, title: text, task, state: "open", updated_at: 1 })));
  const answer = (asked: Envelope): Envelope[] => {
    if (asked.type === "machine_settings_query") {
      const signedIn = SETTINGS.sign_ins.filter(({ state }) => state === "signed_in").map(({ agent }) => agent);
      const answer =
        asked.query.kind === "repos"
          ? { kind: "repos" as const, repos: ["garden"] }
          : asked.query.kind === "agents"
            ? { kind: "agents" as const, agents: signedIn }
            : { kind: "current" as const, ...SETTINGS };
      return [{ id: 0, type: "machine_settings_answer", request: asked.request, machine: asked.machine, workspace: asked.workspace, answer }];
    }
    if (asked.type === "chat_command" && asked.action.kind === "say" && asked.action.task !== undefined) {
      made.push({ chat: asked.chat, task: asked.action.task.id, text: asked.action.text });
      return [
        { id: asked.id, type: "receipt", request: asked.request, state: "accepted" },
        { id: 0, type: "workspace", workspace: asked.workspace, seq: 1, change: { kind: "chat_changed", chat: asked.chat } },
      ];
    }
    return [];
  };
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands", "machine.settings", "workspace.tasks"], answer });
  await openPersonal(served);
  await page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name: "Tasks" }).click();
  await page.getByRole("textbox", { name: "New task" }).fill("Turn the compost");
  await page.getByRole("button", { name: "Add" }).click();
  const start = page.getByRole("button", { name: "Start a chat for Turn the compost" });
  // Listed first: the hold below would catch the add's own request too.
  await start.waitFor();
  // The tasks are held back from the chats' view, so the message goes before it knows the title.
  let release = () => {};
  const held = new Promise<void>((resolve) => {
    release = resolve;
  });
  await page.route("**/api/workspaces/*/tasks", async (route) => {
    await held;
    return route.fallback();
  });
  await start.click();

  const form = page.getByRole("region", { name: "New chat" });
  await form.getByLabel("Repository").waitFor();
  await form.getByRole("combobox", { name: "Message" }).fill("Plan it.");
  await form.getByRole("button", { name: "Send" }).click();
  const link = page.getByRole("link", { name: "Turn the compost" });
  await page.getByRole("heading", { name: "Plan it." }).waitFor();
  release();
  await link.waitFor();
  // A first message needs no agent to start the task's chat.
  const prompt = sent.find((envelope) => envelope.type === "chat_command");
  assert.ok(prompt?.type === "chat_command" && prompt.action.kind === "say");
  // The server gives it the task's title.
  assert.equal(prompt.action.task?.title, "");
  await page.unroute("**/api/workspaces/*/tasks");
  await link.click();
  await page.locator(".tasks li.shown").getByText("Turn the compost").waitFor();
  assert.equal(new URL(page.url()).searchParams.get("task"), prompt.action.task?.id);

  // A chat started from New chat has no task until one is picked by its title; tasks that
  // couldn't be fetched say so there, and come with Try again.
  let failing = true;
  await page.route("**/api/workspaces/*/tasks", (route) => (failing ? route.fulfill({ status: 500 }) : route.fallback()));
  await page.getByRole("navigation", { name: "Sections" }).getByRole("button", { name: "Chats" }).click();
  await page.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: "New chat" }).click();
  await form.getByLabel("Repository").waitFor();
  failing = false;
  await page.locator("section").getByRole("button", { name: "Try again" }).click();
  await form.getByRole("searchbox", { name: "Task (optional)" }).fill("COMPOST");
  await form.getByRole("list", { name: "Matching tasks" }).getByRole("button", { name: "Turn the compost" }).click();
  await form.getByText("Task: Turn the compost").waitFor();
  await form.getByRole("button", { name: "Remove" }).click();
  await form.getByRole("searchbox", { name: "Task (optional)" }).waitFor();
});

test("a message found with ⌘K opens its chat at it, however far back it is", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const last = 600;
  await standIn(page, () => [{ id: "c-1", title: "Garden notes", state: "open", updated_at: 1 }]);
  await page.route("**/api/workspaces/*/search", (route) => {
    const hits = [{ kind: "chat", chat: "c-1", title: "Garden notes", seq: 5, snippet: { text: "Where the compost goes", marks: [] } }];
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ hits }) });
  });
  const windows: number[] = [];
  const answer = (asked: Envelope): Envelope[] => {
    if (asked.type !== "chat_query") {
      return [];
    }
    const { workspace, chat, query } = asked;
    const events = (from: number, to: number) => Array.from({ length: to - from + 1 }, (_, n) => said(from + n));
    if (query.kind === "snapshot") {
      return [{ id: asked.id, type: "chat_answer", workspace, chat, answer: { kind: "snapshot", seq: last, runs: [], waiting: [], events: events(last - 49, last) } }];
    }
    if (query.kind === "window") {
      windows.push(query.before);
      const from = Math.max(1, query.before - query.limit);
      return [{ id: asked.id, type: "chat_answer", workspace, chat, answer: { kind: "window", events: events(from, query.before - 1) } }];
    }
    return [];
  };
  await serveFeed(page, { also: ["workspace.search"], answer });
  await openPersonal(served);

  await page.keyboard.press("ControlOrMeta+k");
  const search = page.getByRole("dialog", { name: "Search" });
  await search.getByRole("combobox", { name: "Search" }).fill("compost");
  await search.getByRole("option", { name: /Garden notes/ }).click();

  const landed = page.locator(".row.landed");
  await landed.getByText("Where the compost goes").waitFor();
  assert.equal(new URL(page.url()).searchParams.get("at"), "5");
  assert.ok(windows.length > 1, `${windows}`);
  assert.equal(await page.locator(".row.landed").count(), 1);
  // The view is at the message found, not at the latest one.
  const atBottom = await page.locator(".transcript").evaluate((element) => element.scrollTop + element.clientHeight >= element.scrollHeight - 2);
  assert.equal(atBottom, false);
  const inView = await landed.evaluate((row) => {
    const scroller = row.closest(".transcript")?.getBoundingClientRect();
    const box = row.getBoundingClientRect();
    return scroller !== undefined && box.top >= scroller.top && box.bottom <= scroller.bottom;
  });
  assert.equal(inView, true);

  // Back at the latest lines without the message named, the same hit lands on it again.
  await page.getByRole("navigation", { name: "Chats" }).getByRole("button", { name: /Garden notes/ }).click();
  await page.waitForURL((url) => !url.searchParams.has("at"));
  await page.locator(".transcript").evaluate((element) => element.scrollTo(0, element.scrollHeight));
  await page.keyboard.press("ControlOrMeta+k");
  await search.getByRole("combobox", { name: "Search" }).fill("compost");
  await search.getByRole("option", { name: /Garden notes/ }).click();
  await landed.waitFor();
  await page.waitForFunction(() => {
    const row = document.querySelector(".row.landed");
    const scroller = row?.closest(".transcript")?.getBoundingClientRect();
    const box = row?.getBoundingClientRect();
    return scroller !== undefined && box !== undefined && box.top >= scroller.top && box.bottom <= scroller.bottom;
  });
});
