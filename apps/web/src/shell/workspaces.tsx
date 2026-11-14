import { type ReactNode, useEffect, useRef, useState } from "react";

import { chatRefOf, workspaceOf } from "../chat-ref.ts";
import { statusText } from "../connection.ts";
import type { ChatId, WorkspaceId, WorkspaceSummary } from "../protocol.ts";
import type { Api } from "./api.ts";
import { useAsk } from "./ask.ts";
import { ChatsNav } from "./chats.tsx";
import { Frame } from "./frame.tsx";
import { STORE, opening, remember } from "./last-workspace.ts";
import { useLoaded } from "./loaded.ts";
import { useBurst, useFeed } from "./feed-hooks.ts";
import { WorkspaceSocket, WorkspaceSocketContext, useWorkspaceSocket } from "./workspace-socket.ts";

/**
 * The signed-in person's workspaces, and the one `?workspace=` names, or else the last one opened
 * here, open on its feed with a selector in the header; `who` is the header's account part. A
 * `page`, such as Settings, shows in their place while they stay open beneath it.
 */
export function Workspaces({ api, who, page }: { api: Api; who: ReactNode; page?: ReactNode }) {
  const { value, problem, load, change } = useLoaded(() => api.workspaces(), api);
  const list = value?.workspaces;
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
  const workspace = list?.find(({ id }) => id === open);
  // Whether the list is on screen, so a workspace made after the person moved on doesn't pull them back.
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
        <Workspace key={workspace.id} api={api} workspace={workspace} chosen={chat} choose={chooseChat} />
      ) : (
        <section className="page workspaces">
          <h1>Workspaces</h1>
          {list === undefined && problem === undefined && <p className="muted">Loading</p>}
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
          {problem !== undefined && (
            <>
              <p role="alert">{problem}</p>
              <button type="button" onClick={() => void load()}>
                Try again
              </button>
            </>
          )}
          {list !== undefined && (
            <NewWorkspace
              api={api}
              made={(made) => {
                change((before) => ({ workspaces: [...(before?.workspaces ?? []), made] }));
                if (listing.current) {
                  choose(made.id);
                }
              }}
            />
          )}
        </section>
      )}
      </div>
      </SocketScope>
    </Frame>
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
          () => api.create(name),
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
  const [scope, setScope] = useState<{ workspace: WorkspaceId; socket: WorkspaceSocket }>();
  if (scope?.workspace !== workspace) {
    setScope(workspace === undefined ? undefined : { workspace, socket: new WorkspaceSocket(workspace) });
  }
  return <WorkspaceSocketContext value={scope?.workspace === workspace ? scope?.socket : undefined}>{children}</WorkspaceSocketContext>;
}

function Workspace({
  api,
  workspace,
  chosen,
  choose,
}: {
  api: Api;
  workspace: WorkspaceSummary;
  chosen: ChatId | undefined;
  choose: (chat: ChatId) => void;
}) {
  const socket = useWorkspaceSocket();
  const { status, offered } = useFeed(workspace.id, api, socket);
  // Whether the server serves chats, known once the feed is welcomed.
  const chats = offered?.includes("workspace.chats");
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
          <p className="muted">{chats === undefined ? "Loading" : "No chats yet. Connect a machine to start one."}</p>
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
