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
  /** Capabilities to offer besides the feed and chats. */
  also?: string[];
  /** What to answer each envelope the page sends after its hello. */
  answer?: (sent: Envelope) => Envelope[];
};

/**
 * Stands in for every workspace's feed: welcomes it offering chats and `also`, then does what
 * `feed` says. Gives the addresses opened and what the page sent after its hellos. Only pages
 * loaded after this have their sockets routed, so it reloads the page.
 */
async function serveFeed(page: Page, { changes = () => [], close = () => false, also = [], answer = () => [] }: Feed = {}): Promise<{ opened: string[]; sent: Envelope[] }> {
  const opened: string[] = [];
  const sent: Envelope[] = [];
  await page.routeWebSocket(/\/api\/workspaces\/[^/]+\/feed/, (socket) => {
    opened.push(new URL(socket.url()).search);
    const nth = opened.length;
    socket.onMessage((text) => {
      const hello = JSON.parse(String(text)) as Envelope;
      if (hello.type !== "hello") {
        sent.push(hello);
        for (const reply of answer(hello)) {
          socket.send(JSON.stringify(reply));
        }
        return;
      }
      const app = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
      const capabilities = ["workspace.feed", "workspace.chats", ...also];
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
  return { opened, sent };
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
  // This server doesn't offer chats, so nobody asks it for them.
  assert.equal(listed(), 0);

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
      { id: "c-2", title: "Sort the pantry", owner: "m-1", machine: "team VPS", state: "full", updated_at: 1_800_000_100 },
      { id: "c-3", title: "Old notes", owner: "m-1", machine: "desktop", state: "detached", updated_at: 1_700_000_000 },
    ],
  }));
  await serveFeed(page);
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  const chats = page.getByRole("navigation", { name: "Chats" });
  await chats.getByText("Sort the pantry").waitFor();

  assert.deepEqual(await chats.locator(".title").allInnerTexts(), ["Sort the pantry", "Merge the shopping lists", "Old notes"]);
  assert.deepEqual(await chats.locator("li .muted").allInnerTexts(), ["team VPS · Full", "laptop", "desktop · Read only"]);
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
});
