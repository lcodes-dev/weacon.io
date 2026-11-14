import assert from "node:assert/strict";
import { test } from "node:test";

import type { Locator, Page } from "playwright-core";

import { fakeServer, start } from "./harness.ts";

test("a dragged range of lines takes a comment that can be edited and removed", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 1), { query: "&workspace=w-1&chat=c-1" });
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByRole("button", { name: "Changes" }).click();
  const file = page.locator(".diff .file").first();
  const marked = markedIn(file);

  // Selecting code picks no lines.
  await file.locator('.line[data-row="2"] > span').click();
  assert.equal(await page.locator(".comment").count(), 0);

  await dragGutter(page, file, 4, 9);
  assert.deepEqual(await marked("selected"), [4, 5, 6, 7, 8, 9]);
  await page.getByRole("textbox", { name: "Lines old 44 to new 45" }).fill("Why gather here?");
  await page.getByRole("button", { name: "Comment", exact: true }).click();

  const comment = file.locator(".comment");
  assert.deepEqual(await comment.locator("p").allInnerTexts(), ["Lines old 44 to new 45", "Why gather here?"]);
  assert.deepEqual(await marked("commented"), [4, 5, 6, 7, 8, 9]);
  assert.ok(await below(comment, file.locator('.line[data-row="9"]')), "the comment isn't under its last line");

  await comment.getByRole("button", { name: "Edit" }).click();
  await page.getByRole("textbox").fill("Why collect here?");
  await page.getByRole("button", { name: "Save" }).click();
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("button", { name: "Changes" }).click();
  assert.equal(await comment.locator(".text").innerText(), "Why collect here?");

  await comment.getByRole("button", { name: "Remove" }).click();
  assert.equal(await page.locator(".comment").count(), 0);
  assert.deepEqual(await marked("commented"), []);
});

test("picking more lines keeps a draft, and nothing moves under a pressed pointer", { timeout: 180_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 1), { query: "&workspace=w-1&chat=c-1" });
  t.after(() => harness.close());
  const { page } = harness;
  await page.getByRole("button", { name: "Changes" }).click();
  const file = page.locator(".diff .file").first();
  const marked = markedIn(file);
  const gutter = { position: { x: 8, y: 10 } };

  await file.locator('.line[data-row="20"]').click(gutter);
  await page.getByRole("textbox").fill("A draft");
  await file.locator('.line[data-row="22"]').click({ ...gutter, modifiers: ["Shift"] });
  assert.deepEqual(await marked("selected"), [20, 21, 22]);
  assert.equal(await page.getByRole("textbox").inputValue(), "A draft");
  assert.ok(await below(file.locator(".comment"), file.locator('.line[data-row="22"]')), "the box didn't move to the picked lines");

  // The box stays open above while lines below are dragged over.
  await dragGutter(page, file, 30, 31);
  assert.deepEqual(await marked("selected"), [30, 31]);
  assert.equal(await page.getByRole("textbox").inputValue(), "A draft");

  await page.keyboard.press("Escape");
  assert.deepEqual(await marked("selected"), []);
  assert.equal(await page.locator(".comment").count(), 0);
});

function markedIn(file: Locator): (mark: string) => Promise<number[]> {
  return (mark) =>
    file.evaluate(
      (section, wanted) => [...section.querySelectorAll(`.line[data-mark="${wanted}"]`)].map((line) => Number(line.getAttribute("data-row"))),
      mark,
    );
}

// Presses on row `from`'s gutter and lets go on row `to`'s.
async function dragGutter(page: Page, file: Locator, from: number, to: number): Promise<void> {
  const gutter = async (row: number) => {
    const box = await file.locator(`.line[data-row="${row}"]`).boundingBox();
    assert.ok(box !== null);
    return { x: box.x + 8, y: box.y + box.height / 2 };
  };
  const start = await gutter(from);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  const end = await gutter(to);
  await page.mouse.move(end.x, end.y, { steps: 5 });
  await page.mouse.up();
}

async function below(upper: Locator, lower: Locator): Promise<boolean> {
  const [comment, line] = await Promise.all([upper.boundingBox(), lower.boundingBox()]);
  return comment !== null && line !== null && Math.abs(comment.y - (line.y + line.height)) < 8;
}
