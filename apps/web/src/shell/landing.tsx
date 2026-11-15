import { type RefObject, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { WorkspaceId, WorkspaceList, WorkspaceSummary } from "../protocol.ts";
import { Invitations } from "./invitations.tsx";

/**
 * Where a person lands with no workspace open: why they left the last one, if they were made to,
 * their invitations, their workspaces to open one, and a form to make another.
 */
export function Landing({
  api,
  loaded,
  listing,
  notice,
  choose,
}: {
  api: Api;
  loaded: Loading<WorkspaceList>;
  listing: RefObject<boolean>;
  notice: string | undefined;
  choose(id: WorkspaceId): void;
}) {
  const list = loaded.value?.workspaces;
  const invitations = loaded.value?.invitations ?? [];
  return (
    <section className="page workspaces">
      {notice !== undefined && <p role="status">{notice}</p>}
      {invitations.length > 0 && (
        <>
          <h1>Invitations</h1>
          <Invitations api={api} invitations={invitations} change={loaded.change} joined={choose} />
        </>
      )}
      {loaded.loading && <p className="muted">Loading</p>}
      {list !== undefined && list.length > 0 && (
        <>
          <h1>Workspaces</h1>
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
        </>
      )}
      {loaded.problem !== undefined && <Problem text={loaded.problem} retry={() => void loaded.load()} />}
      {list !== undefined && (
        <NewWorkspace
          api={api}
          made={(made) => {
            loaded.change((before) => ({ ...before, workspaces: [...(before?.workspaces ?? []), made] }));
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
