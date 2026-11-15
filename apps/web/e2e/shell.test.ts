import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import type { ChatList, Envelope } from "../src/protocol.ts";
import { waitFor } from "./harness.ts";
import { EMAIL, type Served, serve, signIn, signInAgain } from "./server.ts";

const CHANGE_AFTER_MS = 500;

/** What the feed stand-in does beyond welcoming each socket. */
type Feed = {
  /** What to send the nth socket for its workspace, a moment after its welcome. */
  changes?: (opened: number, workspace: string) => Envelope[];
  /** Whether to close the nth socket after its changes. */
  close?: (opened: number) => boolean;
  /** Capabilities to offer besides the feed and chats, or those the nth socket offers. */
  also?: string[] | ((opened: number) => string[]);
  /** What the nth socket answers each envelope the page sends after its hello; `reply` answers later. */
  answer?: (sent: Envelope, opened: number, reply: (envelope: Envelope) => void) => Envelope[];
  /** Whether the nth socket closes once it has answered `sent`. */
  drop?: (sent: Envelope, opened: number) => boolean;
};

/**
 * Stands in for every workspace's feed: welcomes it offering chats and `also`, then does what
 * `feed` says. Gives the addresses opened, what the page sent after its hellos, and a way to
 * close the latest socket. Only pages loaded after this have their sockets routed, so it reloads the page.
 */
async function serveFeed(
  page: Page,
  { changes = () => [], close = () => false, also = [], answer = () => [], drop = () => false }: Feed = {},
): Promise<{ opened: string[]; sent: Envelope[]; closeLatest(): Promise<void> }> {
  const opened: string[] = [];
  const sent: Envelope[] = [];
  let latest: { close(): Promise<void> } | undefined;
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/feed/, (socket) => {
    opened.push(new URL(socket.url()).search);
    latest = socket;
    const nth = opened.length;
    socket.onMessage((text) => {
      const hello = JSON.parse(String(text)) as Envelope;
      if (hello.type !== "hello") {
        sent.push(hello);
        const reply = (envelope: Envelope) => socket.send(JSON.stringify(envelope));
        answer(hello, nth, reply).forEach(reply);
        if (drop(hello, nth)) {
          void socket.close();
        }
        return;
      }
      const app = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
      const capabilities = ["workspace.feed", "workspace.chats", ...(typeof also === "function" ? also(nth) : also)];
      const welcome: Envelope = { id: hello.id, type: "welcome", app, capabilities };
      socket.send(JSON.stringify(welcome));
      const workspace = /workspaces\/([^/]+)\/feed/.exec(socket.url())?.[1] ?? "";
      // Changes come a moment after the welcome, as a server's would once something changes.
      setTimeout(() => {
        for (const change of changes(nth, workspace)) {
          socket.send(JSON.stringify(change));
        }
        if (close(nth)) {
          void socket.close();
        }
      }, CHANGE_AFTER_MS);
    });
  });
  await page.reload();
  return { opened, sent, closeLatest: async () => latest?.close() };
}

// Holds back the next workspace the page makes; the returned function lets it through and waits for the answer.
async function holdCreate(page: Page): Promise<() => Promise<void>> {
  let answer = () => {};
  const held = new Promise<void>((resolve) => {
    answer = resolve;
  });
  await page.route("**/api/workspaces", async (route) => {
    if (route.request().method() === "POST") {
      await held;
    }
    await route.continue();
  });
  return async () => {
    answer();
    await page.waitForResponse((response) => response.request().method() === "POST");
    await page.unroute("**/api/workspaces");
  };
}

// Lists `list(n)` as every workspace's chats the nth time they are asked for; gives how often they were.
async function listChats(page: Page, list: (asked: number) => ChatList): Promise<() => number> {
  let asked = 0;
  await page.route("**/api/workspaces/*/chats", (route) => {
    asked += 1;
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify(list(asked)) });
  });
  return () => asked;
}

test("a person signs in with the code sent to them, stays signed in, and signs out", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await served.admin("add-account", "--email", EMAIL);
  await page.getByLabel("Email").fill(EMAIL);
  await page.getByRole("button", { name: "Send code" }).click();
  await page.getByText(`If ${EMAIL} can sign in here, we sent it a code.`).waitFor();
  await page.getByLabel("Code").fill("00000000");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByText("That code didn't work. Check it, or ask for a new one.").waitFor();
  await page.getByLabel("Code").fill(await served.code(EMAIL));
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
  assert.equal(await page.locator("header .who .muted").innerText(), EMAIL);

  // The session is in a cookie the page's scripts can't read.
  assert.equal(await page.evaluate(() => document.cookie), "");
  await page.reload();
  await page.getByRole("button", { name: "Personal" }).waitFor();

  await page.route("**/api/sign-out", (route) => route.abort());
  await page.getByRole("button", { name: "Sign out" }).click();
  await page.getByRole("alert").getByText("The server can't be reached.").waitFor();
  assert.equal(await page.locator("header .who .muted").innerText(), EMAIL);
  await page.unroute("**/api/sign-out");
  await page.getByRole("button", { name: "Sign out" }).click();
  await page.getByRole("heading", { name: "Sign in" }).waitFor();
  await page.reload();
  await page.getByLabel("Email").waitFor();
});

test("a signed-in person makes a workspace, opens its feed, finds it again after a reload, and moves through history", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);

  await page.getByLabel("Name").fill(" ");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByText("Give it a name of one short line.").waitFor();
  await page.getByLabel("Name").fill("Made-up team");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("heading", { name: "Made-up team" }).waitFor();
  await page.getByText("Connected").waitFor();

  await page.reload();
  await page.getByRole("heading", { name: "Made-up team" }).waitFor();
  await page.getByText("Connected").waitFor();
  await page.getByRole("button", { name: "All workspaces" }).click();
  const names = await page.locator(".workspaces li button").allInnerTexts();
  assert.deepEqual(names.toSorted(), ["Made-up team", "Personal"]);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByText("Connected").waitFor();

  await page.goBack();
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
  await page.goForward();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
});

test("the list takes a workspace made after the person moved on, and copes with a bad address, a failed load and an ended session", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByLabel("Name").fill("Made-up team");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("heading", { name: "Made-up team" }).waitFor();

  // A workspace made after the person moved on joins the list without taking them away.
  await page.getByRole("button", { name: "All workspaces" }).click();
  const release = await holdCreate(page);
  await page.getByLabel("Name").fill("Late team");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await release();
  // Two frames after the answer, the page has handled it.
  await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  assert.equal(await page.getByRole("heading", { name: "Personal" }).count(), 1);
  // One made before the person came back to the list opens once it arrives.
  await page.getByRole("button", { name: "All workspaces" }).click();
  const releaseBack = await holdCreate(page);
  await page.getByLabel("Name").fill("Back team");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "All workspaces" }).click();
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
  await releaseBack();
  await page.getByRole("heading", { name: "Back team" }).waitFor();
  await page.getByRole("button", { name: "All workspaces" }).click();
  await page.getByRole("button", { name: "Late team" }).waitFor();
  await page.goto(new URL("/?workspace=not-mine", page.url()).href);
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();

  await page.route("**/api/workspaces", (route) => route.abort());
  await page.reload();
  await page.getByText("The server can't be reached.").waitFor();
  assert.equal(await page.getByRole("button", { name: "Create" }).count(), 0);
  await page.unroute("**/api/workspaces");
  await page.getByRole("button", { name: "Try again" }).click();
  await page.getByRole("button", { name: "Made-up team" }).waitFor();

  // A session that ends while the page is open sends it back to sign-in.
  await page.route("**/api/workspaces", (route) =>
    route.fulfill({ status: 401, contentType: "application/json", body: '{"code":"sign_in.required","retryable":false}' }),
  );
  await page.reload();
  await page.getByRole("heading", { name: "Sign in" }).waitFor();
});

test("the header switches workspaces, and the page reopens the last one", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const listed = await listChats(page, () => ({ chats: [] }));
  await signIn(served);
  await page.getByLabel("Name").fill("Made-up team");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("heading", { name: "Made-up team" }).waitFor();
  const chats = page.getByRole("navigation", { name: "Chats" });
  await chats.getByText("No chats yet. Connect a machine to start one.").waitFor();
  // The server offers chats, so the workspace asks it for them.
  assert.ok(listed() > 0);

  const selector = page.getByLabel("Workspace");
  assert.deepEqual((await selector.locator("option").allInnerTexts()).toSorted(), ["Made-up team", "Personal"]);
  await selector.selectOption({ label: "Personal" });
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByText("Connected").waitFor();
  assert.match(page.url(), /\?workspace=[A-Za-z0-9_-]+$/);
  await page.goBack();
  await page.getByRole("heading", { name: "Made-up team" }).waitFor();
  await page.goForward();
  await page.getByRole("heading", { name: "Personal" }).waitFor();

  const home = new URL("/", page.url()).href;
  await page.goto(home);
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByText("Connected").waitFor();
  assert.match(page.url(), /\?workspace=/);
  await selector.selectOption({ label: "Made-up team" });
  await page.getByRole("heading", { name: "Made-up team" }).waitFor();
  await page.goBack();
  await page.getByRole("heading", { name: "Personal" }).waitFor();

  await page.getByRole("button", { name: "All workspaces" }).click();
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
  await page.goto(home);
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
});

test("signing out, or a session that ends, forgets the last workspace for the next person", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  const home = new URL("/", page.url()).href;

  const personal = async () => {
    await page.getByRole("button", { name: "Personal" }).click();
    await page.getByText("Connected").waitFor();
  };
  await personal();
  await page.getByRole("button", { name: "Sign out" }).click();
  await page.getByRole("heading", { name: "Sign in" }).waitFor();
  await signInAgain(served);
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();

  // A feed refused for want of a session sends the page back to sign-in, forgetting it too.
  await page.getByLabel("Name").fill("Made-up team");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByText("Connected").waitFor();
  await page.context().clearCookies();
  await page.getByLabel("Workspace").selectOption({ label: "Personal" });
  await page.getByRole("heading", { name: "Sign in" }).waitFor();
  await signInAgain(served);
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();

  // A session found ended when the page loads forgets it as well.
  await personal();
  await page.context().clearCookies();
  await page.reload();
  await page.getByRole("heading", { name: "Sign in" }).waitFor();
  await page.goto(home);
  await signInAgain(served);
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();
});

test("Settings shows the account and this build, and signs out from there", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const email = page.locator("dt:text-is('Email') + dd");
  await signIn(served);
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();

  // A workspace made while Settings covers the list joins it without opening.
  const release = await holdCreate(page);
  await page.getByLabel("Name").fill("Made in passing");
  await page.getByRole("button", { name: "Create" }).click();
  await page.getByRole("button", { name: "Settings" }).click();
  await release();
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Made in passing" }).waitFor();
  await page.getByRole("heading", { name: "Workspaces" }).waitFor();

  await page.getByLabel("Name").fill("Half typed");
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("heading", { name: "Account" }).waitFor();
  assert.equal(await email.innerText(), EMAIL);
  await page.getByRole("heading", { name: "About" }).waitFor();
  assert.deepEqual(await page.locator("header button").allInnerTexts(), ["Back"]);
  assert.equal(await page.locator("header .who .muted").count(), 0);
  assert.equal(await page.getByRole("heading", { name: "Workspaces" }).isVisible(), false);
  await page.getByRole("button", { name: "Back" }).click();
  assert.equal(await page.getByLabel("Name").inputValue(), "Half typed");
  assert.equal(await page.locator("header .who .muted").innerText(), EMAIL);

  await page.getByRole("button", { name: "Settings" }).click();
  await page.route("**/api/sign-out", (route) => route.abort());
  await page.getByRole("button", { name: "Sign out" }).click();
  await page.getByRole("alert").getByText("The server can't be reached.").waitFor();
  assert.equal(await email.innerText(), EMAIL);
  await page.getByRole("button", { name: "Back" }).click();
  assert.equal(await page.getByRole("alert").count(), 0);
  await page.getByRole("button", { name: "Settings" }).click();
  await page.unroute("**/api/sign-out");
  await page.getByRole("button", { name: "Sign out" }).click();
  await page.getByRole("heading", { name: "Sign in" }).waitFor();
  assert.equal(await page.getByRole("button", { name: "Settings" }).count(), 0);
});

test("an address with no account gets the same answer and no code", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const stranger = "nobody@example.com";
  await page.getByLabel("Email").fill(stranger);
  await page.getByRole("button", { name: "Send code" }).click();
  await page.getByText(`If ${stranger} can sign in here, we sent it a code.`).waitFor();
  await page.getByLabel("Code").fill("12345678");
  await page.getByRole("button", { name: "Sign in" }).click();
  await page.getByText("That code didn't work. Check it, or ask for a new one.").waitFor();
  assert.equal(served.letters(), 0);

  await page.getByRole("button", { name: "Use another email" }).click();
  assert.equal(await page.getByLabel("Email").inputValue(), stranger);
});

test("a page that can't reach the server says so, and trying again reaches sign-in", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await page.route("**/api/session", (route) => route.abort());
  await page.reload();
  await page.getByText("The server can't be reached.").waitFor();
  await page.unroute("**/api/session");
  await page.getByRole("button", { name: "Try again" }).click();
  await page.getByLabel("Email").waitFor();
});

test("a workspace lists its chats newest first, and opens one in the address", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await listChats(page, () => ({
    chats: [
      { id: "c-1", title: "Merge the shopping lists", owner: "m-1", machine: "laptop", state: "open", updated_at: 1_800_000_000 },
      { id: "c-2", title: "Sort the pantry", owner: "m-1", machine: "team VPS", state: "full", updated_at: 1_800_000_100, run: "waiting" },
      { id: "c-3", title: "Old notes", owner: "m-1", machine: "desktop", state: "detached", updated_at: 1_700_000_000 },
    ],
  }));
  await serveFeed(page);
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  const chats = page.getByRole("navigation", { name: "Chats" });
  await chats.getByText("Sort the pantry").waitFor();

  assert.deepEqual(await chats.locator(".title").allInnerTexts(), ["Sort the pantry", "Merge the shopping lists", "Old notes"]);
  assert.deepEqual(await chats.locator("li .muted").allInnerTexts(), ["team VPS · Waiting for a person · Full", "laptop", "desktop · Read only"]);
  await chats.getByRole("button", { name: /Merge the shopping lists/ }).click();
  await page.getByRole("heading", { name: "Merge the shopping lists" }).waitFor();
  assert.match(page.url(), /\?workspace=[A-Za-z0-9_-]+&chat=c-1$/);
  assert.equal(await chats.locator("[aria-current]").innerText(), "Merge the shopping lists\nlaptop");
  await page.goBack();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
});

test("a chat change on the feed reloads the list, and a feed that ends opens again after it", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const listed = await listChats(page, (asked) => ({
    chats: asked === 1 ? [] : [{ id: "c-9", title: "A new chat", owner: "m-1", machine: "laptop", state: "open", updated_at: 1 }],
  }));
  const change = (workspace: string, seq: number): Envelope => ({ id: 0, type: "workspace", workspace, seq, change: { kind: "chat_changed", chat: "c-9" } });
  // A burst of two changes and one of another workspace, then, once reopened, the same two again and one more.
  const { opened } = await serveFeed(page, {
    changes: (nth, workspace) => [change(workspace, 7), change(workspace, 8), change("someone-else", 9), ...(nth > 1 ? [change(workspace, 9)] : [])],
    close: (nth) => nth === 1,
  });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  const chats = page.getByRole("navigation", { name: "Chats" });

  await chats.getByText("A new chat").waitFor();
  await page.waitForFunction(() => document.querySelector(".status")?.textContent === "Connected");
  await waitFor(() => listed() === 3);
  assert.deepEqual(opened, ["?since=0", "?since=8"]);
  // Once on opening, once for the first burst, once for the change after the replay.
  assert.equal(listed(), 3);
});

test("the served app forwards its logs and crash reports on a feed that offers it", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { sent } = await serveFeed(page, { also: ["log.forward"] });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByText("Connected").waitFor();
  await page.evaluate(() =>
    setTimeout(() => {
      throw new Error("MARKER-MESSAGE in /home/ada/MARKER-PATH with token MARKER-TOKEN");
    }),
  );
  await waitFor(() => sent.some((envelope) => JSON.stringify(envelope).includes("web.crashed")));

  assert.deepEqual(sent[0], { id: 0, type: "log", level: "info", msg: "app started" });
  assert.doesNotMatch(JSON.stringify(sent), /MARKER/);
});

// A chat's snapshot as the server answers it: a prompt, and Claude Code's answer so far, waiting
// on a permission prompt. Each chat's answer says which chat it is.
function snapshot(asked: Envelope): Envelope[] {
  if (asked.type !== "chat_query" || asked.query.kind !== "snapshot") {
    return [];
  }
  const agent = { kind: "claude" as const, owner: { kind: "member" as const, member: "m-1" }, machine: "laptop" };
  const said = asked.chat === "c-1" ? "Sorting the shelves now." : "Watering the plants now.";
  return [
    {
      id: asked.id,
      type: "chat_answer",
      workspace: asked.workspace,
      chat: asked.chat,
      answer: {
        kind: "snapshot",
        seq: 3,
        runs: [{ run: "r-1", state: "waiting", agent }],
        waiting: [3],
        events: [
          { seq: 1, event: { kind: "prompt", request: "q-1", text: "Sort the pantry." } },
          { seq: 2, event: { kind: "run", update: { run: "r-1", event: { event: "text", text: said } } } },
          { seq: 3, event: { kind: "run", update: { run: "r-1", event: { event: "permission_asked", run: "r-1", prompt: 1, tool: "Bash", input: {} } } } },
        ],
      },
    },
  ];
}

// Answers a chat's snapshot, and takes every command.
function chatServer(asked: Envelope): Envelope[] {
  return asked.type === "chat_command" ? [{ id: asked.id, type: "receipt", request: asked.request, state: "accepted" }] : snapshot(asked);
}

// The commands the page sent, as the chat each names and what it asked.
function commands(sent: Envelope[]): Array<{ workspace: string; chat: string; request: string; action: unknown }> {
  return sent.flatMap((envelope) =>
    envelope.type === "chat_command" ? [{ workspace: envelope.workspace, chat: envelope.chat, request: envelope.request, action: envelope.action }] : [],
  );
}

// The signed-in person's first workspace, and their member in it, as the server says.
async function firstWorkspace(page: Page): Promise<{ id: string; member: string }> {
  return page.evaluate(async () => {
    const answer = (await (await fetch("/api/workspaces")).json()) as { workspaces: Array<{ id: string; member?: string }> };
    return { id: answer.workspaces[0]?.id ?? "", member: answer.workspaces[0]?.member ?? "" };
  });
}

// Signs in and opens the first workspace, whose chats are `chats`, each owned by `owner` of its member.
async function openChats(served: Served, chats: Array<{ id: string; title: string }>, owner: (member: string) => string): Promise<string> {
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).waitFor();
  const { id, member } = await firstWorkspace(page);
  await listChats(page, () => ({ chats: chats.map((chat) => ({ ...chat, owner: owner(member), machine: "laptop", state: "open" as const, updated_at: 1 })) }));
  await page.getByRole("button", { name: "Personal" }).click();
  return id;
}

const nav = (page: Page) => page.getByRole("navigation", { name: "Chats" });

test("a person's own chat shows its transcript and prompts, and takes their messages and Stop", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands"], answer: chatServer });
  const workspace = await openChats(served, [{ id: "c-1", title: "Sort the pantry" }], (member) => member);
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();

  await page.getByText("Sorting the shelves now.").waitFor();
  await page.getByRole("button", { name: "Allow" }).waitFor();
  await page.getByRole("combobox", { name: "Message" }).fill("Use the top shelf too.");
  await page.getByRole("button", { name: "Send" }).click();
  await page.getByRole("button", { name: "Stop" }).click();
  await waitFor(() => commands(sent).length === 2);

  const [prompt, stop] = commands(sent);
  assert.deepEqual([prompt?.action, stop?.action], [{ kind: "prompt", text: "Use the top shelf too." }, { kind: "interrupt" }]);
  for (const command of [prompt, stop]) {
    assert.equal(command?.workspace, workspace);
    assert.equal(command?.chat, "c-1");
  }
  assert.notEqual(prompt?.request, stop?.request);
  assert.ok(sent.some((envelope) => envelope.type === "chat_query" && envelope.query.kind === "subscribe"));
});

test("someone else's chat shows its transcript, without the message box, Stop or the prompts' answers", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands"], answer: snapshot });
  await openChats(served, [{ id: "c-1", title: "Sort the pantry" }], () => "m-someone-else");
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();

  await page.getByText("Sorting the shelves now.").waitFor();
  await page.getByText("Bash").first().waitFor();
  for (const name of ["Send", "Stop", "Allow", "Deny"]) {
    assert.equal(await page.getByRole("button", { name }).count(), 0, name);
  }
  assert.equal(await page.getByRole("combobox", { name: "Message" }).count(), 0);
  assert.deepEqual(commands(sent), []);
});

test("picking another chat shows that one, and sends only to it", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { sent } = await serveFeed(page, { also: ["workspace.chat_commands"], answer: chatServer });
  await openChats(
    served,
    [
      { id: "c-1", title: "Sort the pantry" },
      { id: "c-2", title: "Water the plants" },
    ],
    (member) => member,
  );
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByText("Sorting the shelves now.").waitFor();
  await nav(page).getByRole("button", { name: /Water the plants/ }).click();

  await page.getByText("Watering the plants now.").waitFor();
  assert.equal(await page.getByText("Sorting the shelves now.").count(), 0);
  await page.getByRole("combobox", { name: "Message" }).fill("Only the ferns.");
  await page.getByRole("button", { name: "Send" }).click();
  await waitFor(() => commands(sent).length === 1);
  assert.deepEqual(
    commands(sent).map(({ chat, action }) => ({ chat, action })),
    [{ chat: "c-2", action: { kind: "prompt", text: "Only the ferns." } }],
  );

  // Back on the first chat, its subscription is still the one it had.
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByText("Sorting the shelves now.").waitFor();
  const subscribed = sent.flatMap((envelope) => (envelope.type === "chat_query" && envelope.query.kind === "subscribe" ? [envelope.chat] : []));
  assert.deepEqual(subscribed.toSorted(), ["c-1", "c-2"]);
});

// A changed file of a chat's changes, as a core answers it under `asked`'s request.
function changedFile(asked: Envelope, path: string): Envelope[] {
  if (asked.type !== "diff_query") {
    return [];
  }
  const file = { path, change: { kind: "modified" as const }, content: { kind: "binary" as const } };
  return [{ id: asked.id, type: "diff_answer", workspace: asked.workspace, chat: asked.chat, request: asked.request, answer: { kind: "file", file, files: 2 } }];
}

test("open changes cut short by a lost connection are asked for again once it is back", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  // The first socket sends one of the two files, then drops; the next sends both.
  const { sent } = await serveFeed(page, {
    also: ["diff.view"],
    answer: (asked, nth) => [...snapshot(asked), ...changedFile(asked, "src/pantry.rs"), ...(nth > 1 ? changedFile(asked, "src/shelves.rs") : [])],
    drop: (asked, nth) => nth === 1 && asked.type === "diff_query",
  });
  await openChats(served, [{ id: "c-1", title: "Sort the pantry" }], (member) => member);
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByRole("button", { name: "Changes" }).click();

  await page.getByText("src/shelves.rs").waitFor();
  assert.equal(sent.filter((envelope) => envelope.type === "diff_query").length, 2);
  await page.getByText("src/pantry.rs").waitFor();
});

// A worktree as a core answers it: c-1's has uncommitted work, any other chat has none yet.
function worktreeServer(asked: Envelope): Envelope[] {
  if (asked.type !== "worktree_query") {
    return snapshot(asked);
  }
  if (asked.chat !== "c-1") {
    return [{ id: asked.id, type: "error", code: "worktree.none", retryable: false }];
  }
  const answer = { kind: "state" as const, removed: false, uncommitted: true, unpushed: false };
  return [{ id: asked.id, type: "worktree_answer", workspace: asked.workspace, chat: asked.chat, request: asked.request, answer }];
}

const WORKTREES = ["diff.view", "worktree.manage", "worktree.open_terminal"];

test("a chat's owner gets its worktree's bar, with its title to start a pull request and a terminal where offered", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await serveFeed(page, { also: WORKTREES, answer: worktreeServer });
  await openChats(
    served,
    [
      { id: "c-1", title: "Sort the pantry" },
      { id: "c-2", title: "Water the plants" },
    ],
    (member) => member,
  );
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  const bar = page.getByRole("region", { name: "Worktree" });

  await bar.getByText("Uncommitted changes").waitFor();
  await bar.getByRole("button", { name: "Open terminal" }).waitFor();
  const withButtons = (await bar.boundingBox())?.height;
  await bar.getByRole("button", { name: "Open pull request" }).click();
  assert.equal(await bar.getByLabel("Title").inputValue(), "Sort the pantry");

  await nav(page).getByRole("button", { name: /Water the plants/ }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  await bar.getByText("This chat has no worktree yet.").waitFor();
  assert.equal(await bar.getByRole("button", { name: "Remove" }).count(), 0);
  // The bar is as tall without its buttons as with them, so the diff below doesn't move as they come.
  assert.equal((await bar.boundingBox())?.height, withButtons);
});

test("someone else's chat shows no worktree bar, and asks nothing of it", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const { sent } = await serveFeed(page, { also: WORKTREES, answer: worktreeServer });
  await openChats(served, [{ id: "c-1", title: "Sort the pantry" }], () => "m-someone-else");
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  await page.locator(".chat-changes").waitFor();

  assert.equal(await page.getByRole("region", { name: "Worktree" }).count(), 0);
  assert.deepEqual(
    sent.filter((envelope) => envelope.type === "worktree_query" || envelope.type === "worktree_command"),
    [],
  );
});

test("the worktree's state is asked again on each opening of the changes, and after a failure on Try again", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  // The first ask fails, the next finds no worktree yet, and every later one finds the one made since.
  let asked = 0;
  await serveFeed(page, {
    also: WORKTREES,
    answer: (envelope) => {
      if (envelope.type !== "worktree_query") {
        return snapshot(envelope);
      }
      asked += 1;
      if (asked === 1) {
        return [{ id: envelope.id, type: "error", code: "machine.offline", retryable: true }];
      }
      return asked === 2 ? [{ id: envelope.id, type: "error", code: "worktree.none", retryable: false }] : worktreeServer(envelope);
    },
  });
  await openChats(served, [{ id: "c-1", title: "Sort the pantry" }], (member) => member);
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  const bar = page.getByRole("region", { name: "Worktree" });

  await bar.getByRole("alert").getByText("The worktree's state didn't come. Try again.").waitFor();
  await bar.getByRole("button", { name: "Try again" }).click();
  await bar.getByText("This chat has no worktree yet.").waitFor();
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  await bar.getByText("Uncommitted changes").waitFor();
  assert.equal(asked, 3);
});

test("a pull request's draft stays until the core opens it, and its answer comes while the changes are closed", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  // The first pull request is refused; the second's answer waits until the test sends it.
  let tries = 0;
  let later: (() => void) | undefined;
  await serveFeed(page, {
    also: WORKTREES,
    answer: (asked, _nth, reply) => {
      if (asked.type !== "worktree_command") {
        return worktreeServer(asked);
      }
      tries += 1;
      if (tries === 1) {
        return [{ id: asked.id, type: "error", code: "request.unknown", retryable: false }];
      }
      const url = "https://example.com/pantry/compare";
      later = () => reply({ id: asked.id, type: "worktree_answer", workspace: asked.workspace, chat: asked.chat, request: asked.request, answer: { kind: "pull_request_page", url } });
      return [];
    },
  });
  await openChats(served, [{ id: "c-1", title: "Sort the pantry" }], (member) => member);
  await nav(page).getByRole("button", { name: /Sort the pantry/ }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  const bar = page.getByRole("region", { name: "Worktree" });
  await bar.getByRole("button", { name: "Open pull request" }).click();
  await bar.getByLabel("Description").fill("Sorted by aisle.");
  await bar.getByRole("button", { name: "Open draft pull request" }).click();

  await bar.getByRole("alert").getByText("The pull request wasn't opened.").waitFor();
  assert.equal(await bar.getByLabel("Description").inputValue(), "Sorted by aisle.");
  await bar.getByRole("button", { name: "Open draft pull request" }).click();
  await waitFor(() => later !== undefined);
  // What is typed while it is on its way couldn't be sent, so the form takes nothing then.
  assert.equal(await bar.getByLabel("Description").isDisabled(), true);
  await page.getByRole("button", { name: "Back" }).click();
  later?.();
  await page.getByRole("button", { name: "Changes" }).click();

  const link = bar.getByRole("link", { name: "Open it" });
  await link.waitFor();
  assert.equal(await link.getAttribute("href"), "https://example.com/pantry/compare");
  assert.equal(await bar.getByLabel("Description").count(), 0);
});

test("⌘K searches the open workspace, and choosing a hit goes to its place", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await listChats(page, () => ({
    chats: [{ id: "c-1", title: "Merge the shopping lists", owner: "m-1", machine: "laptop", state: "open", updated_at: 1_800_000_000 }],
  }));
  const asked: string[] = [];
  let answerBread = () => {};
  const bread = new Promise<void>((resolve) => {
    answerBread = resolve;
  });
  await page.route("**/api/workspaces/*/search", async (route) => {
    const text = (JSON.parse(route.request().postData() ?? "{}") as { text: string }).text;
    asked.push(text);
    // The answer for "bread" waits, so the page still holds the one for "milk".
    if (text === "bread") {
      await bread;
    }
    const hits = [
      { kind: "task", task: "t-1", title: "Buy milk", snippet: { text: "Buy milk", marks: [{ start: 4, end: 8 }] } },
      { kind: "chat", chat: "c-1", title: "Merge the shopping lists", seq: 4, snippet: { text: "add the milk", marks: [{ start: 8, end: 12 }] } },
    ];
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ hits }) });
  });
  await serveFeed(page, { also: ["workspace.search"] });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("navigation", { name: "Chats" }).getByText("Merge the shopping lists").waitFor();

  await page.keyboard.press("ControlOrMeta+k");
  const search = page.getByRole("dialog", { name: "Search" });
  await search.getByRole("combobox", { name: "Search" }).fill("milk");
  const results = search.getByRole("option");
  await results.nth(1).waitFor();
  assert.deepEqual(await results.locator(".title").allInnerTexts(), ["Buy milk", "Merge the shopping lists"]);
  assert.deepEqual(await search.locator("mark").allInnerTexts(), ["milk", "milk"]);
  assert.deepEqual(asked, ["milk"]);
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await page.getByRole("heading", { name: "Merge the shopping lists" }).waitFor();
  assert.match(page.url(), /&chat=c-1$/);
  assert.equal(await search.count(), 0);
  await page.keyboard.press("ControlOrMeta+k");
  await search.getByRole("combobox", { name: "Search" }).fill("milk");
  await results.nth(1).waitFor();
  await page.keyboard.press("Enter");
  await page.waitForURL(/&view=tasks$/);
  await page.keyboard.press("ControlOrMeta+k");
  await search.getByRole("combobox", { name: "Search" }).fill("milk");
  await results.nth(1).waitFor();
  await search.getByRole("combobox", { name: "Search" }).fill("bread");
  await results.first().waitFor({ state: "detached" });
  await page.keyboard.press("Enter");
  assert.equal(await search.count(), 1);
  answerBread();
  await results.nth(1).waitFor();
  await page.keyboard.press("Escape");
  await search.waitFor({ state: "detached" });
});

test("the palette keeps focus on its input and the chosen hit in view", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await listChats(page, () => ({ chats: [] }));
  const hits = Array.from({ length: 20 }, (_, n) => ({ kind: "task", task: `t-${n}`, title: `Buy milk ${n}`, snippet: { text: "milk", marks: [{ start: 0, end: 4 }] } }));
  await page.route("**/api/workspaces/*/search", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ hits }) }));
  await serveFeed(page, { also: ["workspace.search"] });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();

  await page.keyboard.press("ControlOrMeta+k");
  const search = page.getByRole("dialog", { name: "Search" });
  const input = search.getByRole("combobox", { name: "Search" });
  await input.fill("milk");
  const last = search.getByRole("option").nth(19);
  await last.waitFor();
  const below = () => last.evaluate((row) => row.getBoundingClientRect().top >= (row.parentElement?.getBoundingClientRect().bottom ?? 0));
  assert.equal(await below(), true);
  for (let n = 0; n < 19; n++) {
    await page.keyboard.press("ArrowDown");
  }
  assert.equal(await last.getAttribute("aria-selected"), "true");
  assert.equal(await below(), false);
  for (const key of ["Tab", "Shift+Tab"]) {
    await page.keyboard.press(key);
    assert.equal(await input.evaluate((element) => element === document.activeElement), true);
  }
  await page.keyboard.press("Escape");
  await search.waitFor({ state: "detached" });
});

test("the palette's symbols go once the server stops offering symbol search", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  const core = { name: "weacon-core", version: "0.1.0", commit: "1a2b3c4" };
  const studio = { id: "mc-1", name: "Studio", os: "linux", key: "ab".repeat(32), paired_at: 1, core, connected: true };
  await page.route("**/api/machines", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ machines: [studio] }) }));
  await page.route("**/api/workspaces/*/search", (route) => route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ hits: [] }) }));
  const symbol = { repo: "garden", kind: "fn" as const, name: "garden::plant", path: "src/lib.rs", line: 8 };
  const feed = await serveFeed(page, {
    also: (opened) => ["workspace.search", ...(opened === 1 ? ["symbols.search"] : [])],
    answer: (sent) =>
      sent.type === "symbols_query"
        ? [{ id: sent.id, type: "symbols_answer", request: sent.request, machine: sent.machine, workspace: sent.workspace, answer: { kind: "found", symbols: [symbol], more: false } }]
        : [],
  });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();

  await page.keyboard.press("ControlOrMeta+k");
  const search = page.getByRole("dialog", { name: "Search" });
  await search.getByRole("combobox", { name: "Search" }).fill("plant");
  const group = search.getByRole("group", { name: "Symbols" });
  await group.getByRole("option").first().waitFor();
  // The feed that comes back after this one no longer offers symbol search.
  await feed.closeLatest();
  await group.waitFor({ state: "detached" });
  assert.equal(await search.count(), 1);
});
