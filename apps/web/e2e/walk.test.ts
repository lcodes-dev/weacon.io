import assert from "node:assert/strict";
import { test } from "node:test";

import { machine } from "./core.ts";
import { serve, signIn } from "./server.ts";

test("a person walks a whole chat on their machine's own core: prompts, Allow, Deny, Stop, a reload and the machine going offline", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  const workspace = new URL(page.url()).searchParams.get("workspace");
  assert.ok(workspace !== null);
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const studio = await machine(served, workspace, "Studio");
  t.after(() => studio.close());
  await page.getByRole("button", { name: "Back" }).click();
  const nav = page.getByRole("navigation", { name: "Chats" });
  // The machines were listed before the core paired.
  await nav.getByRole("button", { name: "Check again" }).click();

  await nav.getByRole("button", { name: "New chat" }).click();
  const form = page.getByRole("region", { name: "New chat" });
  await form.getByLabel("Repository").selectOption("demo");
  await form.getByLabel("Agent").selectOption("claude");
  await form.getByRole("combobox", { name: "Message" }).fill("Run the parser tests");
  await form.getByRole("button", { name: "Send" }).click();
  await page.getByRole("heading", { name: "Run the parser tests" }).waitFor();
  await page.getByText("I'll run them.").waitFor();
  // A page that starts following after the prompt was asked isn't told its tool's name.
  await page.getByText("Asks to use").waitFor();
  await page.getByRole("button", { name: "Allow" }).waitFor();

  // The tool's name isn't kept, so the waiting prompt reads more plainly after a reload.
  await page.reload();
  await page.getByText("Asks to use a tool").waitFor();
  await nav.getByText("ada · Studio · Waiting for a person").waitFor();
  await page.getByRole("button", { name: "Allow" }).click();
  await page.getByText("Done", { exact: true }).waitFor();
  assert.equal(await page.getByText("All 12 tests pass.").count(), 1);
  assert.equal(await page.getByText("Allowed", { exact: true }).count(), 1);

  const box = page.getByRole("combobox", { name: "Message" });
  const send = page.getByRole("button", { name: "Send" });
  await box.fill("Tidy the readme");
  await send.click();
  await page.getByText("Asks to use Write").waitFor();
  await page.getByRole("button", { name: "Deny" }).click();
  await page.getByText("Denied Write").waitFor();
  await page.getByText("Done", { exact: true }).nth(1).waitFor();

  await box.fill("Rewrite the lexer");
  await send.click();
  await page.getByText("Asks to use Edit").waitFor();
  await page.getByRole("button", { name: "Stop" }).click();
  await page.getByText("Stopped", { exact: true }).waitFor();
  await page.getByText("Asked to use Edit").waitFor();

  await studio.stop();
  await box.fill("Run the parser tests");
  await send.click();
  await page.getByText("The machine is offline, so the message wasn't sent. Try again once it's back.").waitFor();
  assert.equal(await box.inputValue(), "Run the parser tests");
  await studio.start();
  await send.click();
  await page.getByText("Asks to use Bash").waitFor();
  await page.getByRole("button", { name: "Allow" }).click();
  await page.getByText("Allowed Bash").waitFor();
  await page.getByText("Done", { exact: true }).nth(2).waitFor();
  assert.equal(await page.getByText("All 12 tests pass.").count(), 2);
  assert.equal(await page.getByText("The machine is offline", { exact: false }).count(), 0);
});
