import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatAction, ChatQueryKind, Envelope } from "../src/protocol.ts";
import { type Harness, fakeServer, start, waitFor } from "./harness.ts";

const CHAT = { query: "&workspace=w-1&chat=c-1" };
// The chat's first prompt, which the first snapshot never holds, and its latest.
const FIRST = "Merge the duplicate lines in the shopping list.";
const LATEST = "Now merge across units too.";

function actions(harness: Harness): ChatAction[] {
  return (harness.received() as Envelope[]).flatMap((sent) => (sent.type === "chat_command" ? [sent.action] : []));
}

function queries(harness: Harness): ChatQueryKind["kind"][] {
  return (harness.received() as Envelope[]).flatMap((sent) => (sent.type === "chat_query" ? [sent.query.kind] : []));
}

test("a chat shows its prompts and labelled turns, and a waiting prompt is answered", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByText(LATEST).waitFor();
  await page.getByText("Shared Codex · team VPS").waitFor();
  await page.getByText("Claude · laptop").first().waitFor();
  await page.getByText("Asks to use Bash").waitFor();

  await page.getByRole("button", { name: "Allow" }).click();
  await page.getByText("The tests pass.").waitFor();
  await page.getByText("Asked to use Bash").waitFor();

  assert.deepEqual(
    actions(harness).filter((action) => action.kind === "permission"),
    [{ kind: "permission", answer: { run: "r-2", prompt: 1, allow: true } }],
  );
  assert.equal(await page.getByRole("button", { name: "Allow" }).count(), 0);
});

test("Stop interrupts the turns that run, then goes away", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  const stop = page.getByRole("button", { name: "Stop" });
  await stop.waitFor();

  await stop.click();
  await waitFor(() => actions(harness).some((action) => action.kind === "interrupt"));
  await page.getByText("Turn ended: interrupted").first().waitFor();
  await stop.waitFor({ state: "detached" });
});

test("after lost events the chat keeps what it shows and takes a new snapshot", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByText("Asks to use Bash").waitFor();
  const box = page.getByRole("combobox", { name: "Message" });

  await box.fill("/lose");
  await box.press("Enter");
  await waitFor(() => queries(harness).length === 4);
  const prompts = page.locator(".row.prompt");
  await prompts.filter({ hasText: "/lose" }).waitFor();
  await box.fill("Still here?");
  await box.press("Enter");
  await prompts.filter({ hasText: "Still here?" }).waitFor();

  assert.deepEqual(queries(harness).filter((kind) => kind !== "window"), ["snapshot", "subscribe", "snapshot", "subscribe"]);
});

test("earlier messages load a page at a time, back to the chat's first prompt", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  const prompts = page.locator(".row.prompt");
  const earlier = page.getByRole("button", { name: "Show earlier messages" });
  await page.getByText("Asks to use Bash").waitFor();
  assert.equal(await prompts.filter({ hasText: FIRST }).count(), 0);

  while ((await earlier.count()) > 0) {
    const [asked, rows] = [queries(harness).length, await page.locator(".row").count()];
    await earlier.click();
    await waitFor(() => queries(harness).length > asked);
    await page.waitForFunction((count) => document.querySelectorAll(".row").length > count, rows);
  }

  assert.equal(await prompts.first().textContent(), FIRST);
  assert.deepEqual(queries(harness).slice(0, 2), ["snapshot", "subscribe"]);
  assert.deepEqual(queries(harness).slice(2).filter((kind) => kind !== "window"), []);
});
