import assert from "node:assert/strict";
import { test } from "node:test";

import { fakeServer, start } from "./harness.ts";

const FILES = 40;

test("Changes asks for the chat's changes once and shows every file, scrolling to the last", { timeout: 180_000 }, async () => {
  const harness = await start(fakeServer(1_000, 1), { query: "&workspace=w-1&chat=c-1" });
  try {
    const { page } = harness;
    await page.getByRole("button", { name: "Changes" }).click();
    await page.waitForFunction((files) => document.querySelectorAll(".diff .file").length === files, FILES);
    await page.getByRole("button", { name: "Back" }).click();
    await page.getByRole("button", { name: "Changes" }).click();
    await page.locator(".diff .file").nth(FILES - 1).scrollIntoViewIfNeeded();

    const shown = await page.evaluate(() => ({
      summary: document.querySelector(".diff > p")?.textContent,
      paths: [...document.querySelectorAll(".diff .path")].map((path) => path.textContent ?? ""),
      first: [...document.querySelectorAll(".diff .file:first-of-type .line")]
        .slice(0, 4)
        .map((line) => [line.getAttribute("data-old"), line.getAttribute("data-new"), line.getAttribute("data-op")]),
      lines: document.querySelectorAll(".diff .line").length,
      last: (({ top, bottom }) => ({ top, bottom }))(document.querySelector(".diff .file:last-of-type")!.getBoundingClientRect()),
      height: innerHeight,
      notes: Object.fromEntries(
        ["src/saddle/part_19.png", "src/orchard/part_39.rs", "src/thistle/part_08.rs", "src/orchard/part_03.rs"].map((path) => {
          const file = [...document.querySelectorAll(".diff .file")].find((each) => each.querySelector(".path")?.textContent === path);
          const notes = [...(file?.querySelectorAll("h2 .muted, p, .note") ?? [])].map((note) => note.textContent);
          return [path, notes];
        }),
      ),
      signs: ["added", "removed", "context"].map(
        (op) => getComputedStyle(document.querySelector(`.diff .line[data-op="${op}"] > span`)!, "::before").content,
      ),
      // A long line's colour reaches as far as its text.
      long: (() => {
        const line = [...document.querySelectorAll<HTMLElement>(".diff .line[data-op='added']")].find((each) => each.textContent!.length > 200)!;
        return { box: line.getBoundingClientRect().width, text: line.firstElementChild!.getBoundingClientRect().width };
      })(),
    }));
    const queries = harness.received().filter((message) => (message as { type?: string }).type === "diff_query");

    assert.equal(shown.summary, "40 files changed");
    assert.deepEqual(shown.paths, [...shown.paths].sort());
    assert.deepEqual(shown.first, [
      ["41", "41", "context"],
      ["42", "42", "context"],
      ["43", "43", "context"],
      ["44", null, "removed"],
    ]);
    assert.equal(shown.lines, 30 * 6 * 24 + 8 * 125);
    assert.deepEqual(shown.notes, {
      "src/saddle/part_19.png": ["Modified", "Binary file."],
      "src/orchard/part_39.rs": ["Modified", "Too large to show."],
      "src/thistle/part_08.rs": ["Renamed from src/thistle/old_08.rs", ...Array.from({ length: 6 }, (_, at) => `From old line ${41 + at * 56}, new line ${41 + at * 58}`)],
      "src/orchard/part_03.rs": ["Added", "From new line 1", "No newline at the end of the file."],
    });
    assert.deepEqual(shown.signs, ['"+ "', '"- "', '"  "']);
    assert.ok(shown.long.box >= shown.long.text, `the line is ${shown.long.box}px wide but its text ${shown.long.text}px`);
    assert.ok(shown.last.top < shown.height && shown.last.bottom > 0, `the last file is at ${JSON.stringify(shown.last)}`);
    const [query, ...more] = queries as { request: string }[];
    assert.deepEqual(more, []);
    assert.match(query?.request ?? "", /^[0-9a-f]{32}$/);
    assert.deepEqual(query, { id: 2, type: "diff_query", request: query?.request, workspace: "w-1", chat: "c-1", query: { kind: "changes" } });
  } finally {
    await harness.close();
  }
});

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
