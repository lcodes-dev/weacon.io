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
  await page.locator(".row.finished", { hasText: "Stopped" }).first().waitFor();
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

test("loading earlier messages keeps a row held back by a selection, and the selection", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(100_000, 1), CHAT);
  t.after(() => harness.close());
  const { page } = harness;
  const earlier = page.getByRole("button", { name: "Show earlier messages" });
  await earlier.waitFor();
  // A row that began before the chat loaded lost its first pieces, and its whole text replaces them.
  const texts = await page.locator(".row.text").count();
  await page.waitForFunction((count) => document.querySelectorAll(".row.text").length > count + 2, texts);
  const selected = await page.evaluate(selectInWritingRow);
  await page.waitForFunction(() => !document.querySelector("[data-picked]")?.hasAttribute("data-writing"));
  const key = await page.evaluate(() => document.querySelector("[data-picked]")?.getAttribute("data-key"));

  assert.equal(await page.evaluate(() => document.getSelection()?.toString()), selected, "before loading");
  const rows = await page.locator(".row").count();
  // A pointer's click would move the selection.
  await page.evaluate(() => document.querySelector<HTMLButtonElement>(".older")?.click());
  await page.waitForFunction((count) => document.querySelectorAll(".row").length > count + 20, rows);

  // The row is the same element, still plain text under the selection, until the selection goes.
  assert.equal(await page.evaluate(() => document.querySelector("[data-picked]")?.getAttribute("data-key")), key);
  assert.equal(await page.evaluate(() => document.getSelection()?.toString()), selected);
  assert.ok(await page.evaluate(() => document.querySelector("[data-picked] > p") === null));
  await page.click("[data-picked]");
  await page.waitForFunction(() => document.querySelector("[data-picked] > p") !== null);
});

// Selects the first piece of the text row still being written, in the frame it is found.
async function selectInWritingRow(): Promise<string> {
  for (let frame = 0; frame < 600; frame += 1) {
    const row = document.querySelector("[data-writing]");
    const piece = row?.firstChild;
    if (row !== null && piece) {
      row.setAttribute("data-picked", "");
      document.getSelection()?.setBaseAndExtent(piece, 0, piece, piece.textContent?.length ?? 0);
      return document.getSelection()?.toString() ?? "";
    }
    await new Promise(requestAnimationFrame);
  }
  throw new Error("no row is being written");
}
