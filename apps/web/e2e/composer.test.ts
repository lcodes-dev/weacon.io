import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../src/protocol.ts";
import { FAKE_CHAT, fakeServer, start, waitFor } from "./harness.ts";

test("Enter during a composition neither sends nor clears, and Enter after it sends", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), { query: FAKE_CHAT });
  t.after(() => harness.close());
  const { page } = harness;
  const box = page.getByRole("combobox", { name: "Message" });
  await page.getByText("Connected").waitFor();
  await box.focus();
  const cdp = await page.context().newCDPSession(page);
  const enter = { key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 };
  // Japanese, then Chinese pinyin, through Chromium's own input method path: compose, press
  // Enter, then commit. The Enter is a bare key press with no text, as an input method leaves it.
  for (const [composed, committed] of [["にほんご", "日本語"], ["zhongwen", "中文"]] as const) {
    await cdp.send("Input.imeSetComposition", { text: composed, selectionStart: composed.length, selectionEnd: composed.length });
    await cdp.send("Input.dispatchKeyEvent", { type: "rawKeyDown", ...enter });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", ...enter });
    assert.equal(await box.inputValue(), composed);
    await cdp.send("Input.insertText", { text: committed });
    await page.keyboard.press("Enter");
    await waitFor(() => prompts(harness.received()).some(({ action }) => action.kind === "say" && action.text === committed));
    // The box clears once the server accepts the message.
    await page.waitForFunction(() => document.querySelector("textarea")?.value === "");
  }

  assert.deepEqual(
    prompts(harness.received()).map((command) => command.action),
    [
      { kind: "say", text: "日本語" },
      { kind: "say", text: "中文" },
    ],
  );

  // A message over the server's limit is kept, and nothing is sent.
  const long = "a".repeat(1_016_000);
  await box.fill(long);
  await box.press("Enter");
  await page.getByText("It's too long to send. Shorten it and try again.").waitFor();
  assert.equal(await box.inputValue(), long);
  await box.press("Backspace");
  assert.equal(await page.getByText("It's too long to send. Shorten it and try again.").count(), 0);

  const send = page.getByRole("button", { name: "Send" });
  await box.fill("   ");
  await box.press("Enter");
  assert.equal(await send.isDisabled(), true);
  await box.fill("Ship it");
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  await page.getByRole("button", { name: "Back" }).click();
  await send.click();
  await waitFor(() => prompts(harness.received()).length === 3);
  assert.deepEqual(prompts(harness.received()).at(-1)?.action, { kind: "say", text: "Ship it" });
  await page.waitForFunction(() => document.querySelector("textarea")?.value === "");
});

test("without a chat, the box keeps what is typed and Send stays off", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0));
  t.after(() => harness.close());
  const { page } = harness;
  const box = page.getByRole("combobox", { name: "Message" });
  await page.getByText("Connected").waitFor();
  await box.fill("hi");
  await box.press("Enter");

  assert.equal(await box.inputValue(), "hi");
  assert.equal(await page.getByRole("button", { name: "Send" }).isDisabled(), true);
  await page.getByText("Pick a chat to send to.").waitFor();
  assert.deepEqual(prompts(harness.received()), []);
});

test("@ offers the writer's own agent, a pick stays one mention, and a message that mentions it is a prompt to it", { timeout: 120_000 }, async (t) => {
  const harness = await start(fakeServer(1_000, 0), { query: FAKE_CHAT });
  t.after(() => harness.close());
  const { page } = harness;
  const box = page.getByRole("combobox", { name: "Message" });
  const options = page.getByRole("option");
  await page.getByText("Connected").waitFor();
  await box.focus();

  await page.keyboard.type("hi @x");
  assert.equal(await options.count(), 0);
  await page.keyboard.press("Backspace");
  await page.keyboard.type("cl");
  assert.deepEqual(await options.allInnerTexts(), ["Claude Code\nYour agent"]);
  await page.keyboard.press("Enter");
  assert.equal(await box.inputValue(), "hi @Claude Code ");
  assert.equal(await options.count(), 0);
  assert.equal(await page.locator(".backdrop mark").innerText(), "@Claude Code");

  // An input method composing the query keeps its Enter.
  await page.keyboard.type("@");
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Input.imeSetComposition", { text: "co", selectionStart: 2, selectionEnd: 2 });
  assert.deepEqual(await options.allInnerTexts(), ["Claude Code\nYour agent"]);
  await cdp.send("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
  assert.equal(await box.inputValue(), "hi @Claude Code @co");
  await cdp.send("Input.insertText", { text: "co" });
  await page.keyboard.press("Tab");
  assert.equal(await box.inputValue(), "hi @Claude Code @Claude Code ");

  // Backspace into a mention takes all of it.
  await page.keyboard.press("Backspace");
  await page.keyboard.press("Backspace");
  assert.equal(await box.inputValue(), "hi @Claude Code ");

  // A click picks without taking the focus or the caret away.
  await page.keyboard.type("and @");
  await options.getByText("Claude Code").click();
  assert.equal(await box.inputValue(), "hi @Claude Code and @Claude Code ");
  assert.equal(await box.evaluate((element: HTMLTextAreaElement) => element === document.activeElement && element.selectionEnd), 33);

  // Composing inside a mention leaves the composition alone, and the mention as plain text.
  await box.evaluate((element: HTMLTextAreaElement) => element.setSelectionRange(8, 8));
  await cdp.send("Input.imeSetComposition", { text: "あ", selectionStart: 1, selectionEnd: 1 });
  assert.equal(await box.inputValue(), "hi @Clauあde Code and @Claude Code ");
  await cdp.send("Input.insertText", { text: "亜" });
  assert.equal(await box.inputValue(), "hi @Clau亜de Code and @Claude Code ");
  assert.deepEqual(await page.locator(".backdrop mark").allInnerTexts(), ["@Claude Code"]);

  // Escape closes the list for this @ only.
  await box.press("End");
  await page.keyboard.type("@cl");
  await page.keyboard.press("Escape");
  assert.equal(await options.count(), 0);
  await page.keyboard.type("x ok");
  await page.keyboard.press("Enter");
  await waitFor(() => prompts(harness.received()).length > 0);
  // The mention left in it calls the agent.
  assert.deepEqual(prompts(harness.received()).at(-1)?.action, {
    kind: "prompt",
    text: "hi @Clau亜de Code and @Claude Code @clx ok",
  });
  await page.waitForFunction(() => document.querySelector("textarea")?.value === "");
  await page.keyboard.type("@");
  assert.equal(await options.count(), 1);
});

type Command = Extract<Envelope, { type: "chat_command" }>;

function prompts(received: unknown[]): Command[] {
  return (received as Envelope[]).filter((record): record is Command => record.type === "chat_command");
}
