import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { ChatId, ChatList } from "../protocol.ts";
import { newestFirst, stateNote } from "./chat-list.ts";

/** The workspace's chats, newest first, with the one `chosen` marked; picking one calls `choose`. */
export function ChatsNav({
  loaded,
  chosen,
  choose,
}: {
  loaded: Loading<ChatList>;
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
}) {
  const chats = loaded.value === undefined ? undefined : newestFirst(loaded.value.chats);
  return (
    <nav className="chats" aria-label="Chats">
      <h2>Chats</h2>
      {loaded.loading && <p className="muted">Loading</p>}
      {chats?.length === 0 && <p className="muted">No chats yet. Connect a machine to start one.</p>}
      {chats !== undefined && chats.length > 0 && (
        <ul>
          {chats.map(({ id, title, machine, state }) => {
            const note = stateNote(state);
            return (
              <li key={id}>
                <button type="button" aria-current={id === chosen ? "page" : undefined} onClick={() => choose(id)}>
                  <span className="title">{title}</span>
                  <span className="muted">
                    {machine}
                    {note !== undefined && ` · ${note}`}
                  </span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
      {loaded.problem !== undefined && <Problem text={loaded.problem} retry={() => void loaded.load()} />}
    </nav>
  );
}
