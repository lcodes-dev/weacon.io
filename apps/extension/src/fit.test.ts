import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import { Failure } from "./core.ts";
import { MAX_FRAME, fit } from "./fit.ts";

const BASE = { request: "p-1", url: "http://localhost/", title: "Home", selector: "p", html: "<p>Hi</p>" };

test("the screenshot halves until the pick fits in a frame", async () => {
  const scales: number[] = [];
  const shoot = async (scale: number) => {
    scales.push(scale);
    return "A".repeat(Math.floor(MAX_FRAME * 3 * scale));
  };

  const pick = await fit(BASE, shoot);

  assert.deepEqual(scales, [1, 0.5, 0.25]);
  assert.equal(pick.screenshot_png?.length, Math.floor(MAX_FRAME * 0.75));
  assert.equal(pick.html, "<p>Hi</p>");
});

test("a pick just under the frame is kept and one at it is shrunk", async () => {
  const empty = JSON.stringify({ id: 2, type: "pick", ...BASE, screenshot_png: "" }).length;
  const room = MAX_FRAME - empty;

  assert.equal((await fit(BASE, async (scale) => "A".repeat(scale === 1 ? room - 1 : 1))).screenshot_png?.length, room - 1);
  assert.equal((await fit(BASE, async (scale) => "A".repeat(scale === 1 ? room : 1))).screenshot_png?.length, 1);
});

test("long text is cut, and no text is left with half a character", async () => {
  const html = `${"a".repeat(256 * 1024 - 1)}\u{1F600} and more`;
  const title = `${"t".repeat(4 * 1024 - 1)}\u{1F600}`;
  const page = { ...BASE, html, title, url: "http://localhost/\ud800", selector: "p\udc00" };

  const pick = await fit(page, async () => "");

  assert.equal(pick.html, `${"a".repeat(256 * 1024 - 1)}�`);
  assert.equal(pick.title, `${"t".repeat(4 * 1024 - 1)}�`);
  assert.equal(pick.url, "http://localhost/�");
  assert.equal(pick.selector, "p�");
});

test("without a screenshot that fits or can be taken, the pick goes without one", async () => {
  const tooLarge = await fit(BASE, async () => "A".repeat(MAX_FRAME));
  const failing = await fit(BASE, async () => {
    throw new Failure("extension.capture_failed");
  });
  const none = await fit(BASE);

  for (const pick of [tooLarge, failing, none]) {
    assert.deepEqual(pick, BASE);
  }
});

test("a pick too large even without a screenshot fails", async () => {
  const html = "\u0001".repeat(256 * 1024);
  const error = await fit({ ...BASE, html }).catch((error: unknown) => error);

  assert.ok(error instanceof Failure && error.code === "extension.pick_too_large");
});

test("the frame limit matches the Rust one", () => {
  const rust = readFileSync(new URL("../../../crates/protocol/src/frame.rs", import.meta.url), "utf8");

  assert.match(rust, /pub const MAX_FRAME: usize = 1024 \* 1024;/);
});
