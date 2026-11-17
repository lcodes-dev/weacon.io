import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import type { InvitationOffer, WorkspaceId, WorkspaceList, WorkspaceSummary } from "../protocol.ts";

/** The person's invitations, each to accept, which opens its workspace. */
export function Invitations({
  api,
  invitations,
  change,
  joined,
}: {
  api: Api;
  invitations: InvitationOffer[];
  /** Changes the workspaces list shown, as an answer takes effect. */
  change(update: (list: WorkspaceList | undefined) => WorkspaceList): void;
  joined(workspace: WorkspaceId): void;
}) {
  const { busy, problem, ask } = useAsk();
  return (
    <>
      <ul aria-label="Invitations">
        {invitations.map((offer) => (
          <li key={offer.workspace}>
            <span>
              {offer.name}
            </span>
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                void ask(
                  () => api.acceptInvitation(offer.workspace),
                  (summary) => {
                    change((list) => withAnswered(list, offer.workspace, summary));
                    joined(summary.id);
                  },
                )
              }
            >
              Accept
            </button>
          </li>
        ))}
      </ul>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </>
  );
}

/** `list` without the invitation to `workspace`, and with `joined`, the workspace it joined. */
function withAnswered(list: WorkspaceList | undefined, workspace: WorkspaceId, joined: WorkspaceSummary): WorkspaceList {
  const workspaces = list?.workspaces ?? [];
  return {
    workspaces: workspaces.some(({ id }) => id === joined.id) ? workspaces : [...workspaces, joined],
    invitations: (list?.invitations ?? []).filter((offer) => offer.workspace !== workspace),
  };
}
