import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { ChatId, ChatList } from "../protocol.ts";
import type { Names } from "../members/names.ts";
import { newestFirst, rowNote } from "./chat-list.ts";

/** The workspace's chats, newest first, with the one `chosen` marked; picking one calls `choose`. */
export function ChatsNav({
  loaded,
  chosen,
  choose,
  names,
}: {
  /** The chats, or none yet while the server is connecting, or none it serves. */
  loaded: Loading<ChatList> | "connecting" | "none";
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
  /** The members' names, for each chat's owner. */
  names: Names;
}) {
  const listed = typeof loaded === "string" ? undefined : loaded;
  const chats = listed?.value === undefined ? undefined : newestFirst(listed.value.chats);
  return (
    <nav className="chats" aria-label="Chats">
      <h2>Chats</h2>
      {(loaded === "connecting" || listed?.loading === true) && <p className="muted">Loading</p>}
      {(loaded === "none" || chats?.length === 0) && <p className="muted">No chats yet. Connect a machine to start one.</p>}
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
      {listed?.problem !== undefined && <Problem text={listed.problem} retry={() => void listed.load()} />}
    </nav>
  );
}
