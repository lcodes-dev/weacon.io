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
  await search.getByRole("combobox", { name: "Search" }).fill("bed water");
  await found.nth(1).waitFor({ state: "detached" });
  assert.deepEqual(await found.locator(".title").allInnerTexts(), ["garden::Bed::water"]);
  await page.keyboard.press("Enter");
  await search.waitFor({ state: "detached" });
  assert.equal(await page.evaluate(() => navigator.clipboard.readText()), "src/bed.rs:12");
});
