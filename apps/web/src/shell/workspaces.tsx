import { useEffect, useRef, useState } from "react";

import { ME } from "../build.ts";
import { Connection, type Status, statusText } from "../connection.ts";
import { workspaceOf } from "../place.ts";
import type { WorkspaceId, WorkspaceSummary } from "../protocol.ts";
import type { Api } from "./api.ts";
import { useAsk } from "./ask.ts";
import { problemText } from "./problems.ts";
import { FEED, feedUrl } from "./workspace-feed.ts";

/** The signed-in person's workspaces, and the one `?workspace=` names, open on its feed. */
export function Workspaces({ api }: { api: Api }) {
  const [list, setList] = useState<WorkspaceSummary[]>();
  const [problem, setProblem] = useState<string>();
  const [open, setOpen] = useState(() => workspaceOf(window.location.search));
  // Each fetch and each made workspace takes a number; an answer older than the list is dropped.
  const changes = useRef(0);
  const load = async () => {
    const asked = ++changes.current;
    setProblem(undefined);
    const answer = await api.workspaces();
    if (asked !== changes.current) {
      return;
    }
    if (answer.ok) {
      setList(answer.value.workspaces);
    } else {
      setProblem(problemText(answer.code));
    }
  };
  useEffect(() => {
    void load();
    const follow = () => setOpen(workspaceOf(window.location.search));
    window.addEventListener("popstate", follow);
    return () => {
      // Answers to an unmounted list are dropped too.
      changes.current++;
      window.removeEventListener("popstate", follow);
    };
  }, [api]);
  const choose = (id: WorkspaceId | undefined) => {
    window.history.pushState(null, "", id === undefined ? window.location.pathname : `?workspace=${id}`);
    setOpen(id);
  };
  const workspace = list?.find(({ id }) => id === open);
  // Whether the list is on screen, so a workspace made after the person moved on doesn't pull them back.
  const listing = useRef(false);
  const shown = workspace?.id;
  useEffect(() => {
    listing.current = shown === undefined;
    return () => {
      listing.current = false;
    };
  }, [shown]);
  if (workspace !== undefined) {
    return <Workspace key={workspace.id} workspace={workspace} leave={() => choose(undefined)} />;
  }
  return (
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
            changes.current++;
            setList((before) => [...(before ?? []), made]);
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

function Workspace({ workspace, leave }: { workspace: WorkspaceSummary; leave(): void }) {
  const status = useFeed(workspace.id);
  return (
    <section className="page">
      <button type="button" onClick={leave}>
        All workspaces
      </button>
      <h1>{workspace.name}</h1>
      <p className="status">{statusText(status)}</p>
    </section>
  );
}

/** Where the feed of `workspace`'s changes stands, while it is open. */
function useFeed(workspace: WorkspaceId): Status {
  const [status, setStatus] = useState<Status>({ state: "connecting" });
  useEffect(() => {
    let current = true;
    const socket = new WebSocket(feedUrl(window.location, workspace));
    new Connection(socket, ME, { status: (next) => current && setStatus(next), message: () => {} }, FEED);
    return () => {
      current = false;
      socket.close();
    };
  }, [workspace]);
  return status;
}
