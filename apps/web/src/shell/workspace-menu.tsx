import { useState } from "react";

import type { Api } from "../api/api.ts";
import type { Loading } from "../api/loaded.ts";
import type { WorkspaceId, WorkspaceList, WorkspaceSummary } from "../protocol.ts";
import { Invitations } from "./invitations.tsx";

/**
 * The foot of the sidebar: the open workspace, which opens a list of the person's other workspaces,
 * their invitations and New workspace, above who is signed in.
 */
export function WorkspaceMenu({
  api,
  loaded,
  workspace,
  email,
  choose,
}: {
  api: Api;
  loaded: Loading<WorkspaceList>;
  workspace: WorkspaceSummary;
  email: string;
  choose(id: WorkspaceId | undefined): void;
}) {
  const [shown, setShown] = useState(false);
  const list = loaded.value;
  const invitations = list?.invitations ?? [];
  const pick = (id: WorkspaceId | undefined) => {
    setShown(false);
    choose(id);
  };
  return (
    <div className="workspace-menu">
      {shown && (
        <div className="menu" aria-label="Workspaces">
          <ul>
            {list?.workspaces.map(({ id, name }) => (
              <li key={id}>
                <button type="button" aria-current={id === workspace.id ? "true" : undefined} onClick={() => pick(id)}>
                  {name}
                </button>
              </li>
            ))}
          </ul>
          {invitations.length > 0 && (
            <>
              <h2>Invitations</h2>
              <Invitations api={api} invitations={invitations} change={loaded.change} joined={pick} />
            </>
          )}
          <button type="button" onClick={() => pick(undefined)}>
            New workspace
          </button>
        </div>
      )}
      <button
        type="button"
        className="current"
        aria-expanded={shown}
        aria-label={`Workspace: ${workspace.name}`}
        onClick={() => {
          setShown(!shown);
          // Opening the list fetches it again, so new invitations show.
          if (!shown) {
            void loaded.load();
          }
        }}
      >
        {workspace.name}
        {invitations.length > 0 && <span className="badge">{invitations.length}</span>}
      </button>
      <p className="muted">{email}</p>
    </div>
  );
}
