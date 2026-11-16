import assert from "node:assert/strict";
import { test } from "node:test";

import type { ChatEntry, MachineSettings } from "../src/protocol.ts";
import { type Outside, core, pair } from "./pairing.ts";
import { serve, signIn } from "./server.ts";

const SETTINGS: MachineSettings = { access: "ask_first", sign_ins: [] };
const SESSION = "0b7c6a52-3f1e-4d2a-9c8b-5e4f3a2b1c0d";

const run = (seq: number, event: Extract<ChatEntry["event"], { kind: "run" }>["update"]["event"]): ChatEntry => ({
  seq,
  event: { kind: "run", update: { run: SESSION, event } },
});

test("a person reads a session they ran outside Weacon, read only, and sees its new lines on refresh", { timeout: 600_000 }, async (t) => {
  const served = await serve();
  t.after(() => served.close());
  const { page } = served;
  await signIn(served);
  await page.getByRole("button", { name: "Personal" }).click();
  await page.getByRole("heading", { name: "Personal" }).waitFor();
  await page.getByRole("button", { name: "Settings", exact: true }).click();
  const { token } = await pair(page);
  const outside: Outside = {
    sessions: [
      { agent: "claude", session: SESSION, folder: "/work/garden", modified_at: 1_700_000_000 },
      { agent: "codex", session: "1c8d7b63-4a2f-4e3b-8d9c-6f5a4b3c2d1e", modified_at: 1_600_000_000 },
    ],
    events: [
      { seq: 1, event: { kind: "outside_prompt", text: "Water the plants" } },
      run(2, { event: "tool_call", call: "c-1", tool: "Bash", input: { command: "ls" } }),
      run(3, { event: "tool_done", call: "c-1", failed: false }),
      run(4, { event: "text", text: "The roses are watered." }),
    ],
  };
  const machine = await core(page, token, SETTINGS, outside);
  t.after(() => machine.close());
  const row = page.getByRole("list", { name: "Machines" }).getByRole("listitem").filter({ hasText: "Studio" });

  await row.getByRole("button", { name: "Studio sessions" }).click();
  const listed = row.getByRole("list", { name: "Sessions" }).getByRole("button");
  await listed.first().waitFor();
  assert.deepEqual(await listed.allInnerTexts(), ["Claude Code in /work/garden", "Codex in an unknown folder"]);

  await listed.first().click();
  const opened = row.getByRole("region", { name: "Claude Code in /work/garden" });
  await opened.getByText("The roses are watered.").waitFor();
  await opened.getByText("Water the plants").waitFor();
  await opened.getByText("Bash").first().waitFor();
  // Read only: nothing to type into, nothing to send.
  assert.equal(await opened.getByRole("textbox").count(), 0);
  assert.equal(await opened.getByRole("button", { name: "Send" }).count(), 0);

  outside.events.push({ seq: 6, event: { kind: "outside_prompt", text: "And the tulips" } });
  await opened.getByRole("button", { name: "Refresh" }).click();
  await opened.getByText("And the tulips").waitFor();
  assert.equal(await opened.getByText("Water the plants").count(), 1);

  await opened.getByRole("button", { name: "Back" }).click();
  await row.getByRole("list", { name: "Sessions" }).waitFor();
});
