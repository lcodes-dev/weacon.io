import { type ReactNode, type RefObject, useEffect, useRef, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { type Loading, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { chatRefOf, workspaceOf } from "../chat-ref.ts";
import { statusText } from "../connection.ts";
import type { Log } from "../log.ts";
import type { ChatId, WorkspaceId, WorkspaceList, WorkspaceSummary } from "../protocol.ts";
import { ChatsNav } from "./chats.tsx";
import { useBurst, useFeed } from "./feed-hooks.ts";
import { Frame } from "./frame.tsx";
import { STORE, opening, remember } from "./last-workspace.ts";
import { WorkspaceSocket, WorkspaceSocketContext, useOffers, useWorkspaceSocket } from "./workspace-socket.ts";

/**
 * The signed-in person's workspaces, and the one `?workspace=` names, or else the last one opened
 * here, open on its feed with a selector in the header; `who` is the header's account part. A
 * `page`, such as Settings, shows in their place while they stay open beneath it.
 */
export function Workspaces({ api, log, who, page }: { api: Api; log: Log; who: ReactNode; page?: ReactNode }) {
  const loaded = useLoaded(() => api.workspaces(), api);
  const list = loaded.value?.workspaces;
  const { open, chat, choose, chooseChat } = useOpenWorkspace();
  const workspace = list?.find(({ id }) => id === open);
  // Whether the list is on screen when a workspace made from it arrives, so one made after the
  // person moved on doesn't pull them back, and one made before they came back does open.
  const listing = useRef(false);
  const shown = workspace?.id;
  const covered = page !== undefined;
  useEffect(() => {
    listing.current = shown === undefined && !covered;
    return () => {
      listing.current = false;
    };
  }, [shown, covered]);
  const selector = list !== undefined && workspace !== undefined && (
    <span className="selector">
      <select aria-label="Workspace" value={workspace.id} onChange={(event) => choose(event.target.value)}>
        {list.map(({ id, name }) => (
          <option key={id} value={id}>
            {name}
          </option>
        ))}
      </select>
      <button type="button" onClick={() => choose(undefined)}>
        All workspaces
      </button>
    </span>
  );
  return (
    <Frame middle={page === undefined && selector} right={who}>
      <SocketScope workspace={workspace?.id}>
        {page}
        <div style={{ display: page === undefined ? "contents" : "none" }}>
          {workspace !== undefined ? (
            <Workspace key={workspace.id} api={api} log={log} workspace={workspace} chosen={chat} choose={chooseChat} />
          ) : (
            <AllWorkspaces api={api} loaded={loaded} listing={listing} choose={choose} />
          )}
        </div>
      </SocketScope>
    </Frame>
  );
}

// The workspace and chat the address names, kept in step with Back and Forward and remembered here.
function useOpenWorkspace() {
  const [open, setOpen] = useState(() => opening(window.location.search, STORE));
  const [chat, setChat] = useState(() => chatRefOf(window.location.search)?.chat);
  useEffect(() => {
    const follow = () => {
      setOpen(workspaceOf(window.location.search));
      setChat(chatRefOf(window.location.search)?.chat);
    };
    window.addEventListener("popstate", follow);
    return () => window.removeEventListener("popstate", follow);
  }, []);
  useEffect(() => {
    remember(STORE, open);
    // One opened from memory takes its place in the address, so Back and links find it.
    if (open !== undefined && workspaceOf(window.location.search) !== open) {
      window.history.replaceState(null, "", `?workspace=${open}`);
    }
  }, [open]);
  const choose = (id: WorkspaceId | undefined) => {
    window.history.pushState(null, "", id === undefined ? window.location.pathname : `?workspace=${id}`);
    setOpen(id);
    setChat(undefined);
  };
  const chooseChat = (id: ChatId) => {
    if (id !== chat) {
      window.history.pushState(null, "", `?workspace=${open}&chat=${id}`);
      setChat(id);
    }
  };
  return { open, chat, choose, chooseChat };
}

// Every workspace, to open one, and a form to make another.
function AllWorkspaces({
  api,
  loaded,
  listing,
  choose,
}: {
  api: Api;
  loaded: Loading<WorkspaceList>;
  listing: RefObject<boolean>;
  choose(id: WorkspaceId): void;
}) {
  const list = loaded.value?.workspaces;
  return (
    <section className="page workspaces">
      <h1>Workspaces</h1>
      {loaded.loading && <p className="muted">Loading</p>}
      {list !== undefined && (
        <ul>
          {list.map(({ id, name, role }) => (
            <li key={id}>
              <button type="button" onClick={() => choose(id)}>
                {name}
              </button>
              {role === "owner" && <span className="muted">Owner</span>}
            </li>
          ))}
        </ul>
      )}
      {loaded.problem !== undefined && <Problem text={loaded.problem} retry={() => void loaded.load()} />}
      {list !== undefined && (
        <NewWorkspace
          api={api}
          made={(made) => {
            loaded.change((before) => ({ workspaces: [...(before?.workspaces ?? []), made] }));
            if (listing.current) {
              choose(made.id);
            }
          }}
        />
      )}
    </section>
  );
}

function NewWorkspace({ api, made }: { api: Api; made(workspace: WorkspaceSummary): void }) {
  const [name, setName] = useState("");
  const { busy, problem, ask } = useAsk();
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void ask(
          () => api.createWorkspace(name),
          (workspace) => {
            setName("");
            made(workspace);
          },
        );
      }}
    >
      <h2>New workspace</h2>
      <label>
        Name
        <input required value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <button type="submit" disabled={busy}>
        Create
      </button>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </form>
  );
}

/**
 * Gives the open workspace's page, and any page shown over it, one socket per open `workspace`,
 * without remounting what it holds when that changes.
 */
function SocketScope({ workspace, children }: { workspace: WorkspaceId | undefined; children: ReactNode }) {
  const [socket, setSocket] = useState<WorkspaceSocket>();
  if (socket?.workspace !== workspace) {
    setSocket(workspace === undefined ? undefined : new WorkspaceSocket(workspace));
  }
  return <WorkspaceSocketContext value={socket?.workspace === workspace ? socket : undefined}>{children}</WorkspaceSocketContext>;
}

function Workspace({
  api,
  log,
  workspace,
  chosen,
  choose,
}: {
  api: Api;
  log: Log;
  workspace: WorkspaceSummary;
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
}) {
  const socket = useWorkspaceSocket();
  const status = useFeed(workspace.id, api, socket, log);
  // Whether the server serves chats, known once the feed is welcomed.
  const chats = useOffers(socket, "workspace.chats");
  const page = (title: string) => (
    <section className="page">
      <h1>{title}</h1>
      <p className="status">{statusText(status)}</p>
    </section>
  );
  if (chats !== true) {
    return (
      <div className="workspace">
        <nav className="chats" aria-label="Chats">
          <h2>Chats</h2>
          <p className="muted">{status.state === "connecting" ? "Loading" : "No chats yet. Connect a machine to start one."}</p>
        </nav>
        {page(workspace.name)}
      </div>
    );
  }
  return <WorkspaceChats api={api} workspace={workspace} chosen={chosen} choose={choose} page={page} />;
}

/** The workspace's chats, kept fresh by its feed, beside the chosen one's `page`. */
function WorkspaceChats({
  api,
  workspace,
  chosen,
  choose,
  page,
}: {
  api: Api;
  workspace: WorkspaceSummary;
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
  page: (title: string) => ReactNode;
}) {
  const socket = useWorkspaceSocket();
  const chats = useLoaded(() => api.chats(workspace.id), workspace.id);
  const reload = useBurst(() => void chats.load());
  useEffect(
    () =>
      socket.subscribe((message) => {
        if (message.type === "workspace" && message.change.kind === "chat_changed") {
          reload();
        }
      }),
    [socket],
  );
  const openChat = chats.value?.chats.find(({ id }) => id === chosen);
  return (
    <div className="workspace">
      <ChatsNav loaded={chats} chosen={chosen} choose={choose} />
      {page(openChat?.title ?? workspace.name)}
    </div>
  );
}
