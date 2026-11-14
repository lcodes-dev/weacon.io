import type { ChatId, ChatList } from "../protocol.ts";
import { newestFirst, stateNote } from "./chat-list.ts";
import type { Loaded } from "./loaded.ts";

/** The workspace's chats, newest first, with the one `chosen` marked; picking one calls `choose`. */
export function ChatsNav({
  loaded,
  chosen,
  choose,
}: {
  loaded: Loaded<ChatList> & { load(): Promise<void> };
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
}) {
  const chats = loaded.value === undefined ? undefined : newestFirst(loaded.value.chats);
  return (
    <nav className="chats" aria-label="Chats">
      <h2>Chats</h2>
      {chats === undefined && loaded.problem === undefined && <p className="muted">Loading</p>}
      {chats?.length === 0 && <p className="muted">No chats yet. Connect a machine to start one.</p>}
      {chats !== undefined && chats.length > 0 && (
        <ul>
          {chats.map(({ id, title, machine, state }) => (
            <li key={id}>
              <button type="button" aria-current={id === chosen ? "page" : undefined} onClick={() => choose(id)}>
                <span className="title">{title}</span>
                <span className="muted">
                  {machine}
                  {stateNote(state) !== undefined && ` · ${stateNote(state)}`}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {loaded.problem !== undefined && (
        <>
          <p role="alert">{loaded.problem}</p>
          <button type="button" onClick={() => void loaded.load()}>
            Try again
          </button>
        </>
      )}
    </nav>
  );
}
