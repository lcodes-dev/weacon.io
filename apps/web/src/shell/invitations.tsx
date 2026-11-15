import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import type { InvitationOffer, WorkspaceId, WorkspaceList, WorkspaceSummary } from "../protocol.ts";
import { roleText } from "../members/roles.ts";

/** The person's invitations, each to accept, which opens its workspace, or hide. */
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
  const drop = (workspace: WorkspaceId, add?: WorkspaceSummary) =>
    change((list) => withAnswered(list, workspace, add));
  return (
    <>
      <ul aria-label="Invitations">
        {invitations.map((offer) => (
          <li key={offer.workspace}>
            <span>
              {offer.name}
              <span className="muted">
                {" "}
                {offer.invited_by === "" ? "Invited" : `${offer.invited_by} invited you`} as {roleText(offer.role).toLowerCase()}
              </span>
            </span>
            <button
              type="button"
              disabled={busy}
              onClick={() =>
                void ask(
                  () => api.acceptInvitation(offer.workspace),
                  (summary) => {
                    drop(offer.workspace, summary);
                    joined(summary.id);
                  },
                )
              }
            >
              Accept
            </button>
            <button type="button" disabled={busy} onClick={() => void ask(() => api.hideInvitation(offer.workspace), () => drop(offer.workspace))}>
              Hide
            </button>
          </li>
        ))}
      </ul>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </>
  );
}

/** `list` without the invitation to `workspace`, and with `joined` once it is accepted. */
export function withAnswered(list: WorkspaceList | undefined, workspace: WorkspaceId, joined?: WorkspaceSummary): WorkspaceList {
  const workspaces = list?.workspaces ?? [];
  return {
    workspaces: joined === undefined || workspaces.some(({ id }) => id === joined.id) ? workspaces : [...workspaces, joined],
    invitations: (list?.invitations ?? []).filter((offer) => offer.workspace !== workspace),
  };
}
