import { type ReactNode, useEffect, useRef, useState } from "react";

import { workspaceOf } from "../chat-ref.ts";
import { Connection, type Status, statusText } from "../connection.ts";
import { ME } from "../me.ts";
import type { WorkspaceId, WorkspaceSummary } from "../protocol.ts";
import type { Api } from "./api.ts";
import { useAsk } from "./ask.ts";
import { Frame } from "./frame.tsx";
import { STORE, opening, remember } from "./last-workspace.ts";
import { useLoaded } from "./loaded.ts";
import { FEED, feedUrl } from "./workspace-feed.ts";

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
      {page}
      <div style={{ display: page === undefined ? "contents" : "none" }}>
      {workspace !== undefined ? (
        <Workspace key={workspace.id} api={api} workspace={workspace} />
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

function Workspace({ api, workspace }: { api: Api; workspace: WorkspaceSummary }) {
  const status = useFeed(workspace.id, api);
  return (
    <div className="workspace">
      <nav className="chats" aria-label="Chats">
        <h2>Chats</h2>
        <p className="muted">No chats yet. Connect a machine to start one.</p>
      </nav>
      <section className="page">
        <h1>{workspace.name}</h1>
        <p className="status">{statusText(status)}</p>
      </section>
    </div>
  );
}

/** Where the feed of `workspace`'s changes stands; when it ends, asking for the session lets a lost one reach sign-in. */
function useFeed(workspace: WorkspaceId, api: Api): Status {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  useEffect(() => {
    let current = true;
    const socket = new WebSocket(feedUrl(window.location, workspace));
    const onStatus = (next: Status) => {
      if (!current) {
        return;
      }
      setStatus(next);
      if (next.state === "ended") {
        void api.session();
      }
    };
    new Connection(socket, ME, { status: onStatus, message: () => {} }, FEED, (run, ms) => setTimeout(run, ms));
    return () => {
      current = false;
      socket.close();
    };
  }, [workspace, api]);
  return status;
}
