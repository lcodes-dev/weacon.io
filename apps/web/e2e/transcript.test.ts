import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import { atBottom } from "../src/transcript/follow.ts";
import { fakeServer, start } from "./harness.ts";

const MESSAGES = 33;

test("text stays selected while the run streams, and following waits for the reader", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(100_000, 1));
  t.after(() => harness.close());
  const { page } = harness;
  // A few more than the selection, so it ends well above the row being written.
  await waitForRows(page, ".row.text", MESSAGES + 6);

  // Finished messages show their Markdown; the session has inline code in each pass.
  assert.ok(await page.evaluate(() => document.querySelector(".row.text p code") !== null));

  const writing = await page.evaluate(selectInWritingRow);
  await page.waitForFunction(() => !document.querySelector("[data-picked]")?.hasAttribute("data-writing"));
  assert.equal(await page.evaluate(() => document.getSelection()?.toString()), writing);
  // The finished row waits as plain text while the selection is in it, then gets its Markdown.
  assert.ok(await page.evaluate(() => document.querySelector("[data-picked] > p") === null));
  // Clicking in it to deselect leaves a caret there, which doesn't hold it.
  await page.click("[data-picked]");
  await page.waitForFunction(() => document.querySelector("[data-picked] > p") !== null);

  const before = await page.evaluate(selectMessages, MESSAGES);
  const shown = await page.evaluate(markShown);
  await waitForRows(page, ".row", before.rows + 200);
  const after = await page.evaluate(() => ({
    selected: document.getSelection()?.toString() ?? "",
    messages: [...document.querySelectorAll(".row.text")].filter((row) => document.getSelection()?.containsNode(row, true))
      .length,
  }));
  assert.ok(before.selected.length > 0);
  assert.equal(after.selected, before.selected);
  assert.equal(after.messages, MESSAGES);
  assertStill(shown, await page.evaluate(shownTop));

  await page.evaluate(() => document.getSelection()?.removeAllRanges());
  await scrollTo(page, "bottom");
  await waitForRows(page, ".row", (await rowCount(page)) + 50);
  assert.ok(atBottom(await page.evaluate(scrollOf)), "it didn't follow once the reader was back at the bottom");

  await scrollTo(page, "up");
  const up = await page.evaluate(markShown);
  await waitForRows(page, ".row", (await rowCount(page)) + 50);
  assertStill(up, await page.evaluate(shownTop));

  // A visit to Settings keeps the reader's place.
  await page.getByRole("button", { name: "Settings" }).click();
  await waitForRows(page, ".row", (await rowCount(page)) + 50);
  await page.getByRole("button", { name: "Back" }).click();
  assertStill(up, await page.evaluate(shownTop));

  // Selecting all in Settings doesn't stop a reader at the bottom from following.
  await scrollTo(page, "bottom");
  await page.getByRole("button", { name: "Settings" }).click();
  await page.keyboard.press("ControlOrMeta+A");
  await waitForRows(page, ".row", (await rowCount(page)) + 50);
  await page.evaluate(() => document.getSelection()?.removeAllRanges());
  // Back right after a row renders, so the message box returns and shrinks the pane before the
  // view's own scroll to that row fires its event.
  await page.evaluate(backAfterNextRow);
  await waitForRows(page, ".row", (await rowCount(page)) + 20);
  assert.ok(atBottom(await page.evaluate(scrollOf)), "it stopped following after a selection in Settings");
});

test("a reader at the bottom of a finished run is still there after Settings", { timeout: 60_000 }, async (t) => {
  const harness = await start(fakeServer(20_000, 0));
  t.after(() => harness.close());
  const { page } = harness;
  await page.waitForSelector(".row.finished");
  // The rows near the end grow to their real height once shown, after the view first followed.
  await page.evaluate(settled);
  assert.ok(atBottom(await page.evaluate(scrollOf)), "the run's end stopped short of the bottom");
  await page.getByRole("button", { name: "Settings" }).click();
  await page.getByRole("button", { name: "Back" }).click();
  await page.evaluate(settled);
  assert.ok(atBottom(await page.evaluate(scrollOf)), "the message box hid the end of the run");
});

// Waits until the transcript's height has held for a few frames.
async function settled(): Promise<void> {
  const height = () => document.querySelector(".transcript")?.scrollHeight;
  for (let still = 0, last = height(); still < 5; ) {
    await new Promise(requestAnimationFrame);
    const now = height();
    still = now === last ? still + 1 : 0;
    last = now;
  }
}

function backAfterNextRow(): Promise<void> {
  return new Promise((resolve) => {
    const view = document.querySelector(".transcript");
    const back = [...document.querySelectorAll("button")].find((button) => button.textContent === "Back");
    if (view === null || back === undefined) {
      throw new Error("no transcript or no Back button");
    }
    const watch = new MutationObserver(() => {
      watch.disconnect();
      back.click();
      resolve();
    });
    watch.observe(view, { childList: true, subtree: true });
  });
}

// Nothing the reader sees moved: the marked row is where it was, give or take subpixel rounding.
function assertStill(before: number, after: number): void {
  assert.ok(Math.abs(after - before) <= 1, `the view moved from ${before} to ${after}`);
}

async function waitForRows(page: Page, selector: string, count: number): Promise<void> {
  await page.waitForFunction(([rows, at]) => document.querySelectorAll(rows).length >= at, [selector, count] as const, {
    timeout: 60_000,
  });
}

async function rowCount(page: Page): Promise<number> {
  return page.evaluate(() => document.querySelectorAll(".row").length);
}

async function scrollTo(page: Page, where: "bottom" | "up"): Promise<void> {
  await page.evaluate(async (to) => {
    const view = document.querySelector(".transcript");
    if (view !== null) {
      view.scrollTop = to === "bottom" ? view.scrollHeight : view.scrollTop - 2_000;
      // A reader's scroll reaches the page with its event; a script's event waits for the next
      // frame, and a row rendering before it would be taken as the reader's.
      view.dispatchEvent(new Event("scroll"));
    }
    await new Promise((resolve) => requestAnimationFrame(resolve));
  }, where);
}

function scrollOf(): { scrollTop: number; scrollHeight: number; clientHeight: number } {
  const view = document.querySelector(".transcript");
  return { scrollTop: view?.scrollTop ?? 0, scrollHeight: view?.scrollHeight ?? 1e9, clientHeight: view?.clientHeight ?? 0 };
}

// Selects the first piece of the text row still being written, and gives what is selected.
// Waits for a row being written and selects it in the same frame, before the row can close.
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

// Selects text rows 1 to `count`, as a drag would. Each function here runs in the page on its own.
async function selectMessages(count: number): Promise<{ selected: string; rows: number }> {
  const texts = document.querySelectorAll(".row.text");
  const first = texts[0];
  const last = texts[count - 1];
  if (first === undefined || last === undefined) {
    throw new Error("the transcript isn't there");
  }
  document.getSelection()?.setBaseAndExtent(first, 0, last, last.childNodes.length);
  // Selected rows are laid out at their real height from the next frame.
  for (let frames = 0; frames < 2; frames += 1) {
    await new Promise((resolve) => requestAnimationFrame(resolve));
  }
  return {
    selected: document.getSelection()?.toString() ?? "",
    rows: document.querySelectorAll(".row").length,
  };
}

// Marks the first row that reaches below the middle of the view, and gives where its top is.
function markShown(): number {
  const view = document.querySelector(".transcript");
  if (view === null) {
    throw new Error("the transcript isn't there");
  }
  const box = view.getBoundingClientRect();
  const middle = box.top + box.height / 2;
  const shown = [...document.querySelectorAll(".row")].find((row) => row.getBoundingClientRect().bottom > middle);
  if (shown === undefined) {
    throw new Error("no row in the middle of the view");
  }
  document.querySelector("[data-shown]")?.removeAttribute("data-shown");
  shown.setAttribute("data-shown", "");
  return shown.getBoundingClientRect().top;
}

function shownTop(): number {
  return document.querySelector("[data-shown]")?.getBoundingClientRect().top ?? Number.NaN;
}
