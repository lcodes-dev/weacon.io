import type { Status } from "../connection.ts";
import type { Place } from "../place.ts";
import type { Envelope, RequestId } from "../protocol.ts";

/** The command that sends `text` to the chat at `place`, under `request`. */
export function prompt(id: number, place: Place, request: RequestId, text: string): Envelope {
  return { id, type: "chat_command", ...place, request, action: { kind: "prompt", text } };
}

/** Why the box can't send to `place` now, or `undefined` when it can. */
export function blocked(status: Status, place: Place | undefined): string | undefined {
  if (place === undefined) {
    return "Pick a chat to send";
  }
  if (status.state !== "live") {
    return "Messages send once the server is connected";
  }
  return status.capabilities.includes("chat.send") ? undefined : "This server can't take messages yet";
}

/** A request id made of `bytes` in hex. */
export function requestId(bytes: Uint8Array): RequestId {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
}
