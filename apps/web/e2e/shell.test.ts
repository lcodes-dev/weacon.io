import assert from "node:assert/strict";
import { test } from "node:test";

import { serve } from "./server.ts";

const EMAIL = "ada@example.com";

test("a person signs in with the code sent to them, stays signed in, and signs out", { timeout: 600_000 }, async () => {
  const served = await serve();
  try {
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
    await page.getByText("You're signed in.").waitFor();
    assert.equal(await page.locator("header .who .muted").innerText(), EMAIL);

    // The session is in a cookie the page's scripts can't read.
    assert.equal(await page.evaluate(() => document.cookie), "");
    await page.reload();
    await page.getByText("You're signed in.").waitFor();

    await page.route("**/api/sign-out", (route) => route.abort());
    await page.getByRole("button", { name: "Sign out" }).click();
    await page.getByRole("alert").getByText("The server can't be reached").waitFor();
    assert.equal(await page.locator("header .who .muted").innerText(), EMAIL);
    await page.unroute("**/api/sign-out");
    await page.getByRole("button", { name: "Sign out" }).click();
    await page.getByRole("heading", { name: "Sign in" }).waitFor();
    await page.reload();
    await page.getByLabel("Email").waitFor();
  } finally {
    await served.close();
  }
});

test("an address with no account gets the same answer and no code", { timeout: 600_000 }, async () => {
  const served = await serve();
  try {
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
  } finally {
    await served.close();
  }
});

test("a page that can't reach the server says so, and trying again reaches sign-in", { timeout: 600_000 }, async () => {
  const served = await serve();
  try {
    const { page } = served;
    await page.route("**/api/session", (route) => route.abort());
    await page.reload();
    await page.getByText("The server can't be reached").waitFor();
    await page.unroute("**/api/session");
    await page.getByRole("button", { name: "Try again" }).click();
    await page.getByLabel("Email").waitFor();
  } finally {
    await served.close();
  }
});
