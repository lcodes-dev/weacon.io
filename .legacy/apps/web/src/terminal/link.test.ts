import assert from "node:assert/strict";
import { test } from "node:test";

import type { Incoming } from "../incoming.ts";
import type { Envelope, TerminalUpdate } from "../protocol.ts";
import { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { terminalLink } from "./link.ts";

function fake(live: boolean) {
  const sent: Envelope[] = [];
  const handles = new Set<(message: Incoming) => void>();
  let ids = 10;
  const link = terminalLink({
    watch: () => () => {},
    connection: 1,
    live,
    send: (envelope) => {
      sent.push(envelope);
      return live;
    },
    subscribe: (handle) => {
      handles.add(handle);
      return () => handles.delete(handle);
    },
    nextId: () => ++ids,
  });
  const hear = (message: Incoming) => handles.forEach((handle) => handle(message));
  return { link, sent, hear, handles };
}

const COMMAND = { terminal: "t-1", machine: "mc-1", workspace: "w-1", action: { kind: "close" } } as const;

test("each command goes out under a new envelope id, and says whether the socket took it", () => {
  const live = fake(true);
  const down = fake(false);

  assert.equal(live.link.send(COMMAND), true);
  assert.equal(live.link.send(COMMAND), true);
  assert.equal(down.link.send(COMMAND), false);

  assert.deepEqual(live.sent, [
    { id: 11, type: "terminal_command", ...COMMAND },
    { id: 12, type: "terminal_command", ...COMMAND },
  ]);
});

test("only terminal updates reach the terminal, until it unsubscribes", () => {
  const { link, hear, handles } = fake(true);
  const heard: TerminalUpdate[] = [];
  const update = { id: 0, type: "terminal", terminal: "t-1", workspace: "w-1", event: { kind: "closed" } } as const;

  const unsubscribe = link.subscribe((got) => heard.push(got));
  hear({ id: 3, type: "error", code: "relay.unknown", retryable: false });
  hear(update);
  unsubscribe();
  hear(update);

  assert.deepEqual(heard, [update]);
  assert.equal(handles.size, 0);
});

test("the terminal hears its connection is lost when it ends or another takes its place, until it stops", () => {
  const ended = new WorkspaceSocket("w-1");
  const replaced = new WorkspaceSocket("w-1");
  const quiet = new WorkspaceSocket("w-1");
  const send = () => {};
  for (const socket of [ended, replaced, quiet]) {
    socket.attach(send);
  }
  const lost: string[] = [];
  terminalLink(ended).lost(() => lost.push("ended"));
  terminalLink(replaced).lost(() => lost.push("replaced"));
  const stop = terminalLink(quiet).lost(() => lost.push("quiet"));

  ended.attach(undefined);
  replaced.attach(() => {});
  stop();
  quiet.attach(undefined);

  assert.deepEqual(lost, ["ended", "replaced"]);
});
