import assert from "node:assert/strict";
import { test } from "node:test";

import type { Page } from "playwright-core";

import { fakeServer, start } from "./harness.ts";

// The made-up change the fake server sends, as crates/testing/src/fake_diff.rs builds it.
const FILES = 40;
const EDITED_FILES = 30;
const HUNKS = 6;
const HUNK_LINES = 24;
// Lines a hunk has on each side: its context, plus its removed or its added lines.
const HUNK_OLD_LINES = 16;
const HUNK_NEW_LINES = 18;
const GAP = 40;
const WHOLE_FILES = 8;
const WHOLE_FILE_LINES = 125;
const BINARY = "src/saddle/part_19.png";
const TOO_LARGE = "src/orchard/part_39.rs";
const RENAMED = "src/thistle/part_08.rs";
const ADDED = "src/orchard/part_03.rs";

test("Changes asks for the chat's changes once and shows every file, scrolling to the last", { timeout: 180_000 }, async () => {
  const harness = await start(fakeServer(1_000, 1), { query: "&workspace=w-1&chat=c-1" });
  try {
    const { page } = harness;
    await page.getByRole("button", { name: "Changes" }).click();
    await page.waitForFunction((files) => document.querySelectorAll(".diff .file").length === files, FILES);
    await page.getByRole("button", { name: "Back" }).click();
    await page.getByRole("button", { name: "Changes" }).click();
    const last = page.locator(".diff .file").nth(FILES - 1);
    await last.scrollIntoViewIfNeeded();
    const paths = await page.locator(".diff .path").allTextContents();
    const hunkStarts = Array.from(
      { length: HUNKS },
      (_, at) => `From old line ${1 + GAP + at * (GAP + HUNK_OLD_LINES)}, new line ${1 + GAP + at * (GAP + HUNK_NEW_LINES)}`,
    );
    const line = (at: number) => `${1 + GAP + at}`;

    assert.equal(await page.locator(".diff > p").textContent(), "40 files changed");
    assert.deepEqual(paths, [...paths].sort());
    assert.deepEqual(await firstLines(page), [
      [line(0), line(0), "context"],
      [line(1), line(1), "context"],
      [line(2), line(2), "context"],
      [line(3), null, "removed"],
    ]);
    assert.equal(await page.locator(".diff .line").count(), EDITED_FILES * HUNKS * HUNK_LINES + WHOLE_FILES * WHOLE_FILE_LINES);
    assert.deepEqual(await notes(page, BINARY), ["Modified", "Binary file."]);
    assert.deepEqual(await notes(page, TOO_LARGE), ["Modified", "Too large to show."]);
    assert.deepEqual(await notes(page, RENAMED), ["Renamed from src/thistle/old_08.rs", ...hunkStarts]);
    assert.deepEqual(await notes(page, ADDED), ["Added", "From new line 1", "No newline at the end of the file."]);
    assert.deepEqual(await signs(page), ['"+ "', '"- "', '"  "']);
    const long = await longLine(page);
    assert.ok(long.box >= long.text, `the line is ${long.box}px wide but its text ${long.text}px`);
    const box = await last.boundingBox();
    const height = page.viewportSize()?.height ?? 0;
    assert.ok(box !== null && box.y < height && box.y + box.height > 0, `the last file is at ${JSON.stringify(box)}`);
    const queries = harness.received().filter((message) => (message as { type?: string }).type === "diff_query");
    const [query, ...more] = queries as { id: number; request: string }[];
    assert.deepEqual(more, []);
    assert.match(query?.request ?? "", /^[0-9a-f]{32}$/);
    // The hello takes id 1; the chat's queries may come first.
    assert.ok((query?.id ?? 0) > 1);
    assert.deepEqual(query, { id: query?.id, type: "diff_query", request: query?.request, workspace: "w-1", chat: "c-1", query: { kind: "changes" } });
  } finally {
    await harness.close();
  }
});

function firstLines(page: Page): Promise<(string | null)[][]> {
  return page.evaluate(() =>
    [...document.querySelectorAll(".diff .file:first-of-type .line")]
      .slice(0, 4)
      .map((line) => [line.getAttribute("data-old"), line.getAttribute("data-new"), line.getAttribute("data-op")]),
  );
}

// What the file at `path` says about itself: how it changed, then its notes in order.
function notes(page: Page, path: string): Promise<string[]> {
  return page
    .locator(".diff .file")
    .filter({ has: page.getByText(path, { exact: true }) })
    .locator("h2 .muted, p, .note")
    .allTextContents();
}

function signs(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    ["added", "removed", "context"].map(
      (op) => getComputedStyle(document.querySelector(`.diff .line[data-op="${op}"] > span`)!, "::before").content,
    ),
  );
}

// A long added line's width beside its text's, so its colour reaches as far as the text.
function longLine(page: Page): Promise<{ box: number; text: number }> {
  return page.evaluate(() => {
    const line = [...document.querySelectorAll<HTMLElement>(".diff .line[data-op='added']")].find((each) => each.textContent!.length > 200)!;
    return { box: line.getBoundingClientRect().width, text: line.firstElementChild!.getBoundingClientRect().width };
  });
}

test("Changes isn't offered without a chat to ask about", { timeout: 120_000 }, async () => {
  const harness = await start(fakeServer(1_000, 0));
  try {
    const { page } = harness;
    await page.getByText("Connected").waitFor();

    assert.equal(await page.getByRole("button", { name: "Changes" }).count(), 0);
  } finally {
    await harness.close();
  }
});
