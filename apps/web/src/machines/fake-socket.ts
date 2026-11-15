import type { Later } from "../connection.ts";
import type { Incoming } from "../incoming.ts";
import type { Envelope } from "../protocol.ts";
import type { MachineSocket } from "./ask-machine.ts";

/** A socket to the open workspace `w-1` whose core the test plays, and a clock it runs by hand. */
export function fake(how: { send?: "throws" | "dropped" } = {}) {
  const sent: Envelope[] = [];
  const handles = new Set<(message: Incoming) => void>();
  const timers: Array<() => void> = [];
  let ids = 10;
  const socket: MachineSocket = {
    workspace: "w-1",
    send: (envelope) => {
      if (how.send === "throws") throw new Error("closed");
      if (how.send === "dropped") return false;
      sent.push(envelope);
      return true;
    },
    subscribe: (handle) => {
      handles.add(handle);
      return () => handles.delete(handle);
    },
    nextId: () => ++ids,
  };
  const later: Later = (run) => {
    timers.push(run);
  };
  return {
    socket,
    later,
    sent,
    deliver: (message: Incoming) => {
      for (const handle of [...handles]) handle(message);
    },
    listeners: () => handles.size,
    expire: () => {
      for (const run of timers) run();
    },
  };
}

