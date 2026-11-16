import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import type { ChatAction, ChatList, Envelope, MachineList, MachineSettings } from "../src/protocol.ts";
import { serveFeed } from "./feed.ts";
import { waitFor } from "./harness.ts";
import { type Served, serve, signIn } from "./server.ts";

const STUDIO = { id: "mc-1", name: "Studio", os: "macos" as const, key: "ab".repeat(32), paired_at: 1 };
const SETTINGS: MachineSettings = {
  access: "ask_first",
  team: false,
  sign_ins: [
    { agent: "claude", state: "signed_in" },
    { agent: "codex", state: "signed_out" },
  ],
};
const AGENT = { kind: "claude" as const, owner: { kind: "member" as const, member: "m-1" }, machine: "Studio" };

/**
 * A stand-in for the server and the machine's core: what a person's first words made, with
 * whether they called the agent, and the refusals still to come.
 */
type Chats = { made: Map<string, string>; called: Set<string>; offline: number; settings?: MachineSettings };

// Answers the machine's settings and repositories, takes a first message, or a first prompt on
// the person's own machine unless it is offline, and plays the made chat: its first words, then,
// when they called the agent, a running run's reply.
function newChatServer(chats: Chats): (asked: Envelope, opened: number, reply: (envelope: Envelope) => void) => Envelope[] {
  return (asked, _opened, reply) => {
    if (asked.type === "machine_settings_query") {
      const answer = asked.query.kind === "repos" ? { kind: "repos" as const, repos: ["kitchen", "garden"] } : { kind: "current" as const, ...(chats.settings ?? SETTINGS) };
      return [{ id: 0, type: "machine_settings_answer", request: asked.request, machine: asked.machine, workspace: asked.workspace, answer }];
    }
    if (asked.type === "chat_command" && asked.action.kind === "say") {
      chats.made.set(asked.chat, asked.action.text);
      return [
        { id: asked.id, type: "receipt", request: asked.request, state: "accepted" },
        { id: 0, type: "workspace", workspace: asked.workspace, seq: 1, change: { kind: "chat_changed", chat: asked.chat } },
      ];
    }
    if (asked.type === "chat_command" && asked.action.kind === "prompt") {
      if (asked.action.machine !== STUDIO.id) {
        return [{ id: asked.id, type: "error", code: "machine.not_found", retryable: false }];
      }
      if (chats.offline > 0) {
        chats.offline -= 1;
        return [{ id: asked.id, type: "error", code: "machine.offline", retryable: true }];
      }
      chats.made.set(asked.chat, asked.action.text);
      chats.called.add(asked.chat);
      return [
        { id: asked.id, type: "receipt", request: asked.request, state: "accepted" },
        { id: 0, type: "workspace", workspace: asked.workspace, seq: 1, change: { kind: "chat_changed", chat: asked.chat } },
      ];
    }
    if (asked.type !== "chat_query") {
      return [];
    }
    const text = chats.made.get(asked.chat) ?? "";
    const { workspace, chat } = asked;
    if (asked.query.kind === "snapshot") {
      // Its first words are a prompt when they called the agent, else a message to the chat.
      const event = chats.called.has(chat) ? { kind: "prompt" as const, request: "q-1", text } : { kind: "message" as const, author: "m-1", text };
      const events = [{ seq: 1, event }];
      return [{ id: asked.id, type: "chat_answer", workspace, chat, answer: { kind: "snapshot", seq: 1, runs: [], waiting: [], events } }];
    }
    if (asked.query.kind === "subscribe" && asked.query.after === 1 && chats.called.has(chat)) {
      // A run says it is running before its first text, as a core's does.
      reply({ id: 0, type: "chat", workspace, chat, entry: { seq: 2, event: { kind: "run_status", status: { run: "r-1", state: "running", agent: AGENT } } } });
      reply({ id: 0, type: "chat", workspace, chat, entry: { seq: 3, event: { kind: "run", update: { run: "r-1", event: { event: "text", text: "Sorting the shelves now." } } } } });
    }
    return [];
  };
}

// The person's machines are `machines`, and the workspace's chats those `chats` made.
async function standIn(page: Page, machines: MachineList, chats: Chats): Promise<void> {
  await page.route("**/api/machines", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(machines) }));
  await page.route("**/api/workspaces/*/chats", async (route) => {
    const member = await page.evaluate(async () => ((await (await fetch("/api/workspaces")).json()) as { workspaces: Array<{ member?: string }> }).workspaces[0]?.member ?? "");
    const list: ChatList = {
      chats: [...chats.made].map(([id, text]) => ({
        id,
        title: text,
        owner: member,
        legs: chats.called.has(id) ? [{ member, machine: STUDIO.name, machine_id: "mc-1", agent: "claude", detached: false }] : [],
        state: "open",
        updated_at: 1,
      })),
    };
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(list) });
  });
}

async function openPersonal(served: Served): Promise<void> {
  await signIn(served);
  await served.page.getByRole("button", { name: "Personal" }).click();
  await served.page.getByRole("heading", { name: "Personal" }).waitFor();
}

function firstPrompts(sent: Envelope[]): Array<{ chat: string; request: string; action: ChatAction }> {
  return sent.flatMap((envelope) => (envelope.type === "chat_command" ? [{ chat: envelope.chat, request: envelope.request, action: envelope.action }] : []));
}

const nav = (page: Page) => page.getByRole("navigation", { name: "Chats" });

// Types a message that calls the person's own agent, after the mention New chat opens the box on
// once an agent is picked.
async function call(page: Page, text: string): Promise<void> {
  const box = page.getByRole("combobox", { name: "Message" });
  await page.waitForFunction(() => document.querySelector("textarea")?.value === "@Claude Code ");
  await box.press("End");
  await page.keyboard.type(text);
}

test("from an empty workspace, a person starts a chat that calls their agent on their connected machine and reads its reply", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const chats: Chats = { made: new Map(), called: new Set(), offline: 0 };
  await standIn(page, { machines: [{ ...STUDIO, connected: true }] }, chats);
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands", "machine.settings"], answer: newChatServer(chats) });
  await openPersonal(served);
  await nav(page).getByText("No chats yet.").waitFor();
  await nav(page).getByRole("button", { name: "New chat" }).click();

  const form = page.getByRole("region", { name: "New chat" });
  await form.getByRole("group", { name: "Your agent in this chat" }).waitFor();
  await form.getByLabel("Repository").selectOption("garden");
  // Only the agents signed in for this workspace on the machine are offered.
  await form.getByRole("button", { name: "Sign in from Settings" }).waitFor();
  assert.deepEqual(await form.getByLabel("Agent").locator("option").allTextContents(), ["Claude Code"]);
  // Signed in from Settings, the agent can be picked once the person is back.
  chats.settings = { ...SETTINGS, sign_ins: [{ agent: "claude", state: "signed_in" }, { agent: "codex", state: "signed_in" }] };
  await form.getByRole("button", { name: "Sign in from Settings" }).click();
  await page.getByRole("heading", { name: "Machines" }).waitFor();
  await page.getByRole("button", { name: `${STUDIO.name} settings`, expanded: true }).waitFor();
  await page.getByRole("button", { name: "Back" }).click();
  await form.getByLabel("Agent").selectOption("codex");
  await form.getByRole("button", { name: "Sign in from Settings" }).waitFor({ state: "detached" });
  await form.getByLabel("Agent").selectOption("claude");
  assert.equal(await form.getByLabel("Machine").inputValue(), STUDIO.id);
  await call(page, "Sort the pantry.");
  await form.getByRole("button", { name: "Send" }).click();

  await page.getByText("Sorting the shelves now.").waitFor();
  await page.getByRole("heading", { name: "@Claude Code Sort the pantry." }).waitFor();
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).waitFor();
  const [prompt] = firstPrompts(sent);
  assert.deepEqual(prompt?.action, { kind: "prompt", text: "@Claude Code Sort the pantry.", machine: STUDIO.id, repo: "garden", agent: "claude" });
  assert.equal(new URL(page.url()).searchParams.get("chat"), prompt?.chat);
  assert.equal(firstPrompts(sent).length, 1);
});

test("with no machine paired, New chat says what calling an agent lacks, and still starts a chat with a plain message", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const chats: Chats = { made: new Map(), called: new Set(), offline: 0 };
  await standIn(page, { machines: [] }, chats);
  let failed = false;
  await page.route("**/api/machines", async (route) => {
    if (failed) {
      return route.fallback();
    }
    failed = true;
    return route.fulfill({ status: 500 });
  });
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands", "machine.settings"], answer: newChatServer(chats) });
  await openPersonal(served);
  await nav(page).getByRole("button", { name: "New chat" }).click();
  const form = page.getByRole("region", { name: "New chat" });

  await form.getByText("Pair a machine in Settings to call your own agent.").waitFor();
  assert.equal(await form.getByRole("group", { name: "Your agent in this chat" }).count(), 0);
  // Without an agent, @ offers nothing, and the message goes to the chat's people.
  const box = form.getByRole("combobox", { name: "Message" });
  await box.focus();
  await page.keyboard.type("@");
  assert.equal(await page.getByRole("option").count(), 0);
  await box.fill("Who waters the plants?");
  await form.getByRole("button", { name: "Send" }).click();
  await page.getByRole("heading", { name: "Who waters the plants?" }).waitFor();
  assert.deepEqual(firstPrompts(sent).map(({ action }) => action), [{ kind: "say", text: "Who waters the plants?" }]);
  // In the chat, adding an agent says the same, with the way to Settings.
  await page.getByRole("button", { name: "Add your agent" }).click();
  await page.getByText("Pair a machine in Settings to call your own agent.").waitFor();
  await page.getByRole("button", { name: "Open Settings" }).click();
  await page.getByRole("heading", { name: "Machines" }).waitFor();
});

test("with the machine offline, the first prompt is kept and can be sent again once it is back", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const chats: Chats = { made: new Map(), called: new Set(), offline: 1 };
  await standIn(page, { machines: [{ ...STUDIO, connected: true }] }, chats);
  const { sent, opened, closeLatest } = await serveFeed(page, { also: ["workspace.chat_commands", "machine.settings"], answer: newChatServer(chats) });
  await openPersonal(served);
  await nav(page).getByRole("button", { name: "New chat" }).click();

  const form = page.getByRole("region", { name: "New chat" });
  const box = form.getByRole("combobox", { name: "Message" });
  await form.getByLabel("Repository").waitFor();
  await call(page, "Water the plants.");
  await form.getByRole("button", { name: "Send" }).click();
  await form.getByText("The machine is offline, so the chat wasn't started. Try again once it's back.").waitFor();
  assert.equal(await box.inputValue(), "@Claude Code Water the plants.");
  // The view and its text stay while the feed reconnects.
  await closeLatest();
  await waitFor(() => opened.length === 2);
  await page.getByText("Connected").first().waitFor();
  assert.equal(await box.inputValue(), "@Claude Code Water the plants.");

  await form.getByRole("button", { name: "Send" }).click();
  await page.getByText("Sorting the shelves now.").waitFor();
  await waitFor(() => firstPrompts(sent).length === 2);
  const [refused, taken] = firstPrompts(sent);
  assert.deepEqual(taken?.action, refused?.action);
  // The refused prompt made no chat, so the one sent again makes its own.
  assert.notEqual(taken?.chat, refused?.chat);
  assert.equal(new URL(page.url()).searchParams.get("chat"), taken?.chat);
});
