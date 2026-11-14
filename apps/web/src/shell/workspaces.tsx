import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";

import { chatRefOf, workspaceOf } from "../chat-ref.ts";
import { Connection, type Status, statusText } from "../connection.ts";
import { ME } from "../me.ts";
import type { Incoming } from "../incoming.ts";
import type { ChatId, WorkspaceChange, WorkspaceId, WorkspaceSummary } from "../protocol.ts";
import type { Api } from "./api.ts";
import { useAsk } from "./ask.ts";
import { Frame } from "./frame.tsx";
import { STORE, opening, remember } from "./last-workspace.ts";
import { useLoaded } from "./loaded.ts";
import { ChatsNav } from "./chats.tsx";
import { FEED, feedUrl, retryAfter } from "./workspace-feed.ts";
import { FeedSocket, WorkspaceSocketContext } from "./workspace-socket.ts";

/**
 * The signed-in person's workspaces, and the one `?workspace=` names, or else the last one opened
 * here, open on its feed with a selector in the header; `who` is the header's account part. A
 * `page`, such as Settings, shows in their place while they stay open beneath it.
 */
export function Workspaces({ api, who, page }: { api: Api; who: ReactNode; page?: ReactNode }) {
  const { value, problem, load, change } = useLoaded(() => api.workspaces(), api);
  const list = value?.workspaces;
  const [open, setOpen] = useState(() => opening(window.location.search, STORE));
  useEffect(() => {
    const follow = () => setOpen(workspaceOf(window.location.search));
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
  };
  const workspace = list?.find(({ id }) => id === open);
  // The open workspace's one socket, which its page and any page shown over it share.
  const socket = useMemo(() => new FeedSocket(), [workspace?.id]);
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
      <WorkspaceSocketContext value={workspace === undefined ? undefined : socket}>
      {page}
      <div style={{ display: page === undefined ? "contents" : "none" }}>
      {workspace !== undefined ? (
        <Workspace key={workspace.id} api={api} workspace={workspace} socket={socket} />
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
      </WorkspaceSocketContext>
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

function Workspace({ api, workspace, socket }: { api: Api; workspace: WorkspaceSummary; socket: FeedSocket }) {
  const chats = useLoaded(() => api.chats(workspace.id), workspace.id);
  const reload = useBurst(() => void chats.load());
  const status = useFeed(workspace.id, api, socket, (change) => {
    if (change.kind === "chat_changed") {
      reload();
    }
  });
  const [chosen, setChosen] = useState(() => chatRefOf(window.location.search)?.chat);
  useEffect(() => {
    const follow = () => setChosen(chatRefOf(window.location.search)?.chat);
    window.addEventListener("popstate", follow);
    return () => window.removeEventListener("popstate", follow);
  }, []);
  const choose = (chat: ChatId) => {
    window.history.pushState(null, "", `?workspace=${workspace.id}&chat=${chat}`);
    setChosen(chat);
  };
  const open = chats.value?.chats.find(({ id }) => id === chosen);
  return (
    <div className="workspace">
      <ChatsNav loaded={chats} chosen={chosen} choose={choose} />
      <section className="page">
        <h1>{open?.title ?? workspace.name}</h1>
        <p className="status">{statusText(status)}</p>
      </section>
    </div>
  );
}

// How long a burst of changes waits before its one reload, as when a reopened feed catches up.
const BURST_MS = 250;

/** `run`, at most once per burst of calls. */
function useBurst(run: () => void): () => void {
  const latest = useRef(run);
  latest.current = run;
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  return () => {
    if (timer.current === undefined) {
      timer.current = setTimeout(() => {
        timer.current = undefined;
        latest.current();
      }, BURST_MS);
    }
  };
}

/**
 * Where the feed of `workspace`'s changes stands, telling `changed` each one and `socket` every
 * message. When it ends, asking for the session lets a lost one reach sign-in, and the feed opens
 * again after the last change it told, waiting longer after each failure.
 */
function useFeed(workspace: WorkspaceId, api: Api, socket: FeedSocket, changed: (change: WorkspaceChange) => void): Status {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  const latest = useRef(changed);
  latest.current = changed;
  useEffect(() => {
    let current = true;
    let since = 0;
    let failed = 0;
    let opened: WebSocket | undefined;
    let retry: ReturnType<typeof setTimeout> | undefined;
    const open = () => {
      const fresh = new WebSocket(feedUrl(window.location, workspace, since));
      opened = fresh;
      const listener = {
        status(next: Status) {
          if (!current) {
            return;
          }
          setStatus(next);
          if (next.state === "live") {
            failed = 0;
            socket.attach((envelope) => connection.send(envelope));
          } else if (next.state === "ended") {
            socket.attach(undefined);
            void api.session();
            failed += 1;
            retry = setTimeout(open, retryAfter(failed));
          }
        },
        message(envelope: Incoming) {
          if (!current) {
            return;
          }
          if (envelope.type === "workspace" && envelope.workspace === workspace && envelope.seq > since) {
            since = envelope.seq;
            latest.current(envelope.change);
          }
          socket.heard(envelope);
        },
      };
      const connection = new Connection(fresh, ME, listener, FEED, (run, ms) => setTimeout(run, ms));
    };
    open();
    return () => {
      current = false;
      clearTimeout(retry);
      socket.attach(undefined);
      opened?.close();
    };
  }, [workspace, api, socket]);
  return status;
}
