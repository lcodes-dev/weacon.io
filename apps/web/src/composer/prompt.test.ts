import assert from "node:assert/strict";
import { test } from "node:test";

import { isId } from "../place.ts";
import { blocked, prompt, requestId } from "./prompt.ts";

test("a prompt is a chat command for the chat, under its request", () => {
  assert.deepEqual(prompt(3, { workspace: "w-1", chat: "c-1" }, "r-1", "日本語"), {
    id: 3,
    type: "chat_command",
    workspace: "w-1",
    chat: "c-1",
    request: "r-1",
    action: { kind: "prompt", text: "日本語" },
  });
});

test("the box sends only to a chat, on a live connection to a server that takes messages", () => {
  const place = { workspace: "w-1", chat: "c-1" };
  const server = { name: "weacon-server", version: "0.0.0", commit: "0000000" };
  const live = (capabilities: string[]) => ({ state: "live" as const, server, capabilities });

  assert.equal(blocked(live(["chat.send"]), place), undefined);
  assert.equal(blocked(live(["chat.send"]), undefined), "Pick a chat to send");
  assert.equal(blocked(live(["run.stream"]), place), "This server can't take messages yet");
  assert.equal(blocked({ state: "connecting" }, place), "Messages send once the server is connected");
  assert.equal(blocked({ state: "ended", code: "connection.lost" }, place), "Messages send once the server is connected");
});

test("a request id is its bytes in hex, and a well-formed id", () => {
  const id = requestId(Uint8Array.from({ length: 16 }, (_, at) => at * 17));
  assert.equal(id, "00112233445566778899aabbccddeeff");
  assert.equal(isId(id), true);
});
