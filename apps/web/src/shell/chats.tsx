import type { ReactNode } from "react";

import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { ChatId, ChatList } from "../protocol.ts";
import type { Names } from "../members/names.ts";
import { newestFirst, rowNote } from "./chat-list.ts";

/**
 * The workspace's chats, newest first, with the one `chosen` marked; picking one calls `choose`.
 * New chat, first, calls `start`, where one can be started; `children` come between it and the list.
 */
export function ChatsNav({
  loaded,
  chosen,
  choose,
  names,
  start,
  children,
}: {
  loaded: Loading<ChatList>;
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
  /** The members' names, for each chat's owner. */
  names: Names;
  start: (() => void) | undefined;
  children?: ReactNode;
}) {
  const chats = loaded.value === undefined ? undefined : newestFirst(loaded.value.chats);
  return (
    <nav className="chats" aria-label="Chats">
      <button type="button" className="primary" disabled={start === undefined} onClick={start}>
        New chat
      </button>
      {children}
      <h2>Chats</h2>
      {loaded.loading && <p className="muted">Loading</p>}
      {chats?.length === 0 && <p className="muted">No chats yet.</p>}
      {chats !== undefined && chats.length > 0 && (
        <ul>
          {chats.map((chat) => (
            <li key={chat.id}>
              <button type="button" aria-current={chat.id === chosen ? "page" : undefined} onClick={() => choose(chat.id)}>
                <span className="title">{chat.title}</span>
                <span className="muted">{rowNote(chat, names)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {loaded.problem !== undefined && <Problem text={loaded.problem} retry={() => void loaded.load()} />}
    </nav>
  );
}
