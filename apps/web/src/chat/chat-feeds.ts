import type { ChatRef } from "../chat-ref.ts";
import { route } from "../incoming.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { LiveChat } from "./live-chat.ts";
import { START, type Sync } from "./sync.ts";

/** What the chats of a socket need of it. */
export type Feed = Pick<WorkspaceSocket, "subscribe" | "send" | "nextId" | "connection">;

type Held = { chat: LiveChat; connection: number; screen: ((sync: Sync) => void) | undefined; stop: () => void };

/**
 * The chats kept in step over one socket: at most one subscription per chat and connection, as the
 * server keeps one, so a chat shown again reuses its own. A chat no screen shows takes its log's
 * entries but drops its runs' updates.
 */
export class ChatFeeds {
  #feed: Feed;
  #schedule: () => (flush: () => void) => void;
  #held = new Map<string, Held>();

  /** `schedule` makes each chat's flush timing, as `nextFrame` does. */
  constructor(feed: Feed, schedule: () => (flush: () => void) => void) {
    this.#feed = feed;
    this.#schedule = schedule;
  }

  /**
   * Shows the chat at `ref` through `screen` until `hide` is called, asking for it unless this
   * connection already keeps it.
   */
  show(ref: ChatRef, screen: (sync: Sync) => void): { chat: LiveChat; hide: () => void } {
    const held = this.#take(ref);
    held.screen = screen;
    const { sync } = held.chat;
    if (sync.state.kind === "asking" || sync.state.kind === "live") {
      screen(sync);
    } else {
      held.chat.open();
    }
    const hide = () => {
      if (held.screen === screen) {
        held.screen = undefined;
      }
    };
    return { chat: held.chat, hide };
  }

  // The chat this connection keeps for `ref`, or a fresh one that starts from what an earlier one showed.
  #take(ref: ChatRef): Held {
    const key = `${ref.workspace}/${ref.chat}`;
    const before = this.#held.get(key);
    const connection = this.#feed.connection;
    if (before?.connection === connection) {
      return before;
    }
    before?.stop();
    const wiring = {
      id: () => this.#feed.nextId(),
      send: (envelope: Parameters<Feed["send"]>[0]) => void this.#feed.send(envelope),
      schedule: this.#schedule(),
      show: (sync: Sync) => held.screen?.(sync),
    };
    const chat = new LiveChat(ref, wiring, before?.chat.sync ?? START);
    const unsubscribe = this.#feed.subscribe((message) =>
      route(message, { run: (run) => held.screen !== undefined && chat.push(run), chat: (said) => chat.push(said) }),
    );
    const held: Held = {
      chat,
      connection,
      screen: undefined,
      stop: () => {
        unsubscribe();
        chat.close();
      },
    };
    this.#held.set(key, held);
    return held;
  }
}
