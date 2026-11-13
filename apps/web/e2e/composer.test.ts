import assert from "node:assert/strict";
import { test } from "node:test";

import type { Envelope } from "../src/protocol.ts";
import { fakeServer, start, waitFor } from "./harness.ts";

test("Enter during a composition neither sends nor clears, and Enter after it sends", { timeout: 120_000 }, async () => {
  const harness = await start(fakeServer(1_000, 0), { query: "&workspace=w-1&chat=c-1" });
  try {
    const { page } = harness;
    const box = page.getByRole("textbox", { name: "Message" });
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
    const box = page.getByRole("textbox", { name: "Message" });
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

type Command = Extract<Envelope, { type: "chat_command" }>;

function prompts(received: unknown[]): Command[] {
  return (received as Envelope[]).filter((record): record is Command => record.type === "chat_command");
}
