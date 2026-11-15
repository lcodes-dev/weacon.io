import { useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { MemberAction, MemberList, MemberSummary } from "../protocol.ts";
import type { OpenWorkspace } from "../shell/workspaces.tsx";
import { useOffers } from "../shell/workspace-socket.ts";
import { roleText } from "./roles.ts";

/** Whether other people are in the workspace `list` lists: more than one member now. */
export function isTeam(list: MemberList): boolean {
  return list.members.filter((member) => member.role !== undefined).length > 1;
}

/**
 * Settings' Members: who is in the open workspace, past members marked, and for its owner its
 * invitations, a form to invite and Remove; each member may leave.
 */
export function MembersSection({ api, open, members }: { api: Api; open: OpenWorkspace; members: Loading<MemberList> }) {
  const team = useOffers(open.socket, "workspace.team");
  const changing = useAsk();
  if (!team) {
    return null;
  }
  const { workspace } = open;
  const list = members.value;
  const owner = workspace.role === "owner";
  const act = (action: MemberAction, done?: () => void) =>
    void changing.ask(
      () => api.changeMembers(workspace.id, action),
      () => {
        void members.load();
        done?.();
      },
    );
  return (
    <section className="members" aria-label="Members">
      <h2>Members</h2>
      {members.loading && <p className="muted">Loading</p>}
      {members.problem !== undefined && <Problem text={members.problem} retry={() => void members.load()} />}
      {list !== undefined && (
        <ul aria-label="Members">
          {list.members.map((member) => (
            <Member key={member.id} member={member} me={member.id === workspace.member} removes={owner} busy={changing.busy} act={act} />
          ))}
        </ul>
      )}
      {list?.invitations !== undefined && list.invitations.length > 0 && (
        <>
          <h3>Invitations</h3>
          <ul aria-label="Invitations">
            {list.invitations.map((invitation) => (
              <li key={invitation.member}>
                {invitation.email} <span className="muted">ends {ending(invitation.expires_at)}</span>{" "}
                <Confirm label="Revoke" question={`Revoke the invitation to ${invitation.email}?`} busy={changing.busy} confirm={() => act({ kind: "revoke", member: invitation.member })} />
              </li>
            ))}
          </ul>
        </>
      )}
      {owner && <Invite busy={changing.busy} invite={(email, done) => act({ kind: "invite", email }, done)} />}
      {changing.problem !== undefined && <p role="alert">{changing.problem}</p>}
      {!owner && (
        <Confirm label="Leave" question={`Leave ${workspace.name}?`} busy={changing.busy} confirm={() => act({ kind: "leave" }, () => open.left(`You left ${workspace.name}.`))} />
      )}
    </section>
  );
}

function Member({
  member,
  me,
  removes,
  busy,
  act,
}: {
  member: MemberSummary;
  me: boolean;
  removes: boolean;
  busy: boolean;
  act(action: MemberAction): void;
}) {
  const { id, name, role } = member;
  return (
    <li>
      {name}
      {me && " (you)"} <span className="muted">{role === undefined ? "Past member" : roleText(role)}</span>{" "}
      {removes && role === "member" && <Confirm label="Remove" question={`Remove ${name} from this workspace?`} busy={busy} confirm={() => act({ kind: "remove", member: id })} />}
    </li>
  );
}

function Invite({ busy, invite }: { busy: boolean; invite(email: string, done: () => void): void }) {
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState<string>();
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        setSent(undefined);
        invite(email.trim(), () => {
          setSent(`Invited ${email.trim()}. They see the invitation once they sign in.`);
          setEmail("");
        });
      }}
    >
      <h3>Invite someone</h3>
      <label>
        Email
        <input type="email" required value={email} onChange={(event) => setEmail(event.target.value)} />
      </label>
      <button type="submit" disabled={busy}>
        Invite
      </button>
      {sent !== undefined && <p role="status">{sent}</p>}
    </form>
  );
}

/** A button that asks once before it does what it says. */
function Confirm({ label, question, busy, confirm }: { label: string; question: string; busy: boolean; confirm(): void }) {
  const [asking, setAsking] = useState(false);
  if (!asking) {
    return (
      <button type="button" disabled={busy} onClick={() => setAsking(true)}>
        {label}
      </button>
    );
  }
  return (
    <span role="group" aria-label={question}>
      {question}{" "}
      <button
        type="button"
        onClick={() => {
          setAsking(false);
          confirm();
        }}
      >
        {label}
      </button>{" "}
      <button type="button" onClick={() => setAsking(false)}>
        Cancel
      </button>
    </span>
  );
}

// When an invitation ends, as a short date.
function ending(seconds: number): string {
  return new Date(seconds * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}
