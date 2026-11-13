import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../src/protocol.ts";
import { fakeServer, start, waitFor } from "./harness.ts";

test("Enter during a composition neither sends nor clears, and Enter after it sends", { timeout: 120_000 }, async () => {
  const harness = await start(fakeServer(1_000, 0), { query: "&workspace=w-1&chat=c-1" });
  try {
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
      await waitFor(() => prompts(harness.received()).some(({ action }) => action.kind === "prompt" && action.text === committed));
      // The box clears once the server accepts the prompt.
      await page.waitForFunction(() => document.querySelector("textarea")?.value === "");
    }

    assert.deepEqual(
      prompts(harness.received()).map((command) => command.action),
      [
        { kind: "prompt", text: "日本語" },
        { kind: "prompt", text: "中文" },
      ],
    );

    // A prompt over the server's limit is kept, and nothing is sent.
    const long = "a".repeat(17_000);
    await box.fill(long);
    await box.press("Enter");
    await page.getByText("Too long to send; shorten it and try again").waitFor();
    assert.equal(await box.inputValue(), long);

    const send = page.getByRole("button", { name: "Send" });
    await box.fill("   ");
    await box.press("Enter");
    assert.equal(await send.isDisabled(), true);
    await box.fill("Ship it");
    await page.getByRole("button", { name: "Settings" }).click();
    await page.getByRole("button", { name: "Back" }).click();
    await send.click();
    await waitFor(() => prompts(harness.received()).length === 3);
    assert.deepEqual(prompts(harness.received()).at(-1)?.action, { kind: "prompt", text: "Ship it" });
    await page.waitForFunction(() => document.querySelector("textarea")?.value === "");
  } finally {
    await harness.close();
  }
});

test("without a chat, the box keeps what is typed and Send stays off", { timeout: 120_000 }, async () => {
  const harness = await start(fakeServer(1_000, 0));
  try {
    const { page } = harness;
    const box = page.getByRole("combobox", { name: "Message" });
    await page.getByText("Connected").waitFor();
    await box.fill("hi");
    await box.press("Enter");

    assert.equal(await box.inputValue(), "hi");
    assert.equal(await page.getByRole("button", { name: "Send" }).isDisabled(), true);
    await page.getByText("Pick a chat to send").waitFor();
    assert.deepEqual(prompts(harness.received()), []);
  } finally {
    await harness.close();
  }
});

test("@ offers people and agents, and a pick stays one mention", { timeout: 120_000 }, async () => {
  const harness = await start(fakeServer(1_000, 0), { query: "&workspace=w-1&chat=c-1" });
  try {
    const { page } = harness;
    const box = page.getByRole("combobox", { name: "Message" });
    const options = page.getByRole("option");
    await page.getByText("Connected").waitFor();
    await box.focus();

    await page.keyboard.type("hi @ju");
    assert.deepEqual(await options.allInnerTexts(), ["Jun Sato\nPerson"]);
    await page.keyboard.press("Enter");
    assert.equal(await box.inputValue(), "hi @Jun Sato ");
    assert.equal(await options.count(), 0);
    assert.equal(await page.locator(".backdrop mark").innerText(), "@Jun Sato");

    // An input method composing the query keeps its Enter.
    await page.keyboard.type("@");
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Input.imeSetComposition", { text: "co", selectionStart: 2, selectionEnd: 2 });
    assert.deepEqual(await options.allInnerTexts(), ["Claude Code\nAgent", "Codex\nAgent"]);
    await cdp.send("Input.dispatchKeyEvent", { type: "rawKeyDown", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    await cdp.send("Input.dispatchKeyEvent", { type: "keyUp", key: "Enter", code: "Enter", windowsVirtualKeyCode: 13 });
    assert.equal(await box.inputValue(), "hi @Jun Sato @co");
    await cdp.send("Input.insertText", { text: "co" });
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("Tab");
    assert.equal(await box.inputValue(), "hi @Jun Sato @Codex ");

    // Backspace into a mention takes all of it.
    await page.keyboard.press("Backspace");
    await page.keyboard.press("Backspace");
    assert.equal(await box.inputValue(), "hi @Jun Sato ");

    // A click picks without taking the focus or the caret away.
    await page.keyboard.type("and @");
    await options.getByText("Lena Brandt").click();
    assert.equal(await box.inputValue(), "hi @Jun Sato and @Lena Brandt ");
    assert.equal(await box.evaluate((element: HTMLTextAreaElement) => element === document.activeElement && element.selectionEnd), 30);

    // Composing inside a mention leaves the composition alone, and the mention as plain text.
    await box.evaluate((element: HTMLTextAreaElement) => element.setSelectionRange(8, 8));
    await cdp.send("Input.imeSetComposition", { text: "あ", selectionStart: 1, selectionEnd: 1 });
    assert.equal(await box.inputValue(), "hi @Jun あSato and @Lena Brandt ");
    await cdp.send("Input.insertText", { text: "亜" });
    assert.equal(await box.inputValue(), "hi @Jun 亜Sato and @Lena Brandt ");
    assert.deepEqual(await page.locator(".backdrop mark").allInnerTexts(), ["@Lena Brandt"]);

    // Escape closes the list for this @ only.
    await box.press("End");
    await page.keyboard.type("@le");
    await page.keyboard.press("Escape");
    assert.equal(await options.count(), 0);
    await page.keyboard.type("x ok");
    await page.keyboard.press("Enter");
    await waitFor(() => prompts(harness.received()).length > 0);
    assert.deepEqual(prompts(harness.received()).at(-1)?.action, {
      kind: "prompt",
      text: "hi @Jun 亜Sato and @Lena Brandt @lex ok",
    });
    await page.waitForFunction(() => document.querySelector("textarea")?.value === "");
    await page.keyboard.type("@");
    assert.equal(await options.count(), 5);
  } finally {
    await harness.close();
  }
});

type Command = Extract<Envelope, { type: "chat_command" }>;

function prompts(received: unknown[]): Command[] {
  return (received as Envelope[]).filter((record): record is Command => record.type === "chat_command");
}
