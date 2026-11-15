import assert from "node:assert/strict";
import { test } from "node:test";

import type { MachineSettings, Symbol as CodeSymbol } from "../src/protocol.ts";
import { core, pair } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

const SETTINGS: MachineSettings = { access: "ask_first", team: false, sign_ins: [] };

const SYMBOLS: CodeSymbol[] = [
  { repo: "garden", kind: "struct", name: "garden::Bed", path: "src/bed.rs", line: 3 },
  { repo: "garden", kind: "fn", name: "garden::Bed::water", path: "src/bed.rs", line: 12 },
  { repo: "garden", kind: "fn", name: "garden::plant", path: "src/lib.rs", line: 8 },
];

test("⌘K finds the symbols on the person's machine, and choosing one copies where it is", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin: new URL(page.url()).origin });
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings" }).click();
  const { token } = await pair(page);
  const machine = await core(page, token, SETTINGS, undefined, SYMBOLS);
  t.after(() => machine.close());
  await page.getByRole("button", { name: "Back" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();

  await page.keyboard.press("ControlOrMeta+k");
  const search = page.getByRole("dialog", { name: "Search" });
  await search.getByRole("combobox", { name: "Search" }).fill("bed");
  const found = search.getByRole("group", { name: "Symbols" }).getByRole("option");
  await found.nth(1).waitFor();
  assert.deepEqual(await found.locator(".title").allInnerTexts(), ["garden::Bed", "garden::Bed::water"]);
  // The workspace's own hits for "bed water" come after the symbol is chosen, and don't move the choice.
  let answerHits = () => {};
  const held = new Promise<void>((resolve) => {
    answerHits = resolve;
  });
  await page.route("**/api/workspaces/*/search", async (route) => {
    await held;
    const hits = [{ kind: "task", task: "t-1", title: "Water the bed", snippet: { text: "Water the bed", marks: [] } }];
    return route.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ hits }) });
  });
  await search.getByRole("combobox", { name: "Search" }).fill("bed water");
  // The list for "bed" goes once the words change, so this is the answer for "bed water".
  await found.first().waitFor();
  assert.deepEqual(await found.locator(".title").allInnerTexts(), ["garden::Bed::water"]);
  await page.keyboard.press("ArrowDown");
  answerHits();
  await search.getByRole("option", { name: /Water the bed/ }).waitFor();
  await page.keyboard.press("Enter");
  await search.waitFor({ state: "detached" });
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "src/bed.rs:12");

  // A copy that fails says so, and the palette stays open.
  await page.evaluate(() => {
    navigator.clipboard.writeText = () => Promise.reject(new Error("denied"));
  });
  await page.keyboard.press("ControlOrMeta+k");
  await search.getByRole("combobox", { name: "Search" }).fill("plant");
  await found.first().waitFor();
  await found.first().click();
  await search.getByRole("alert").filter({ hasText: "The place couldn't be copied." }).waitFor();

  // A copy that ends after the words changed neither closes the palette nor says it failed.
  await page.evaluate(() => {
    const late = window as unknown as { finish?: () => void };
    navigator.clipboard.writeText = () =>
      new Promise((resolve) => {
        late.finish = () => resolve();
      });
  });
  await found.first().click();
  await search.getByRole("combobox", { name: "Search" }).fill("bed");
  await page.evaluate(() => (window as unknown as { finish: () => void }).finish());
  await found.nth(1).waitFor();
  assert.equal(await search.count(), 1);
  assert.equal(await search.getByRole("alert").count(), 0);

  // Nor does one that ends after the palette closed and opened again.
  await found.first().click();
  await search.getByRole("combobox", { name: "Search" }).press("Escape");
  await search.waitFor({ state: "detached" });
  await page.keyboard.press("ControlOrMeta+k");
  await search.getByRole("combobox", { name: "Search" }).waitFor();
  await page.evaluate(() => (window as unknown as { finish: () => void }).finish());
  await search.getByRole("combobox", { name: "Search" }).fill("bed");
  await found.nth(1).waitFor();
  assert.equal(await search.count(), 1);
});
