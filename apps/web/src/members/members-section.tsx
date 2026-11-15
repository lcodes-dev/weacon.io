import { useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import type { Loading } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import type { MemberAction, MemberList, MemberSummary, WorkspaceRole } from "../protocol.ts";
import type { OpenWorkspace } from "../shell/workspaces.tsx";
import { useOffers } from "../shell/workspace-socket.ts";
import { manages, mayManage, roleText } from "./roles.ts";

/** Whether other people are in the workspace `list` lists: more than one member now. */
export function isTeam(list: MemberList): boolean {
  return list.members.filter((member) => member.role !== undefined).length > 1;
}

/**
 * Settings' Members: who is in the open workspace and with what role, past members marked, and
 * for its owner and admins its invitations and a form to invite; each may leave but the owner.
 */
export function MembersSection({ api, open, members }: { api: Api; open: OpenWorkspace; members: Loading<MemberList> }) {
  const team = useOffers(open.socket, "workspace.team");
  const changing = useAsk();
  if (!team) {
    return null;
  }
  const { workspace } = open;
  const list = members.value;
  const role = list?.members.find(({ id }) => id === workspace.member)?.role ?? workspace.role;
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
            <Member key={member.id} member={member} me={member.id === workspace.member} by={role} busy={changing.busy} act={act} />
          ))}
        </ul>
      )}
      {list?.invitations !== undefined && list.invitations.length > 0 && (
        <>
          <h3>Invitations</h3>
          <ul aria-label="Invitations">
            {list.invitations.map((invitation) => (
              <li key={invitation.member}>
                {invitation.email} <span className="muted">{roleText(invitation.role)}, ends {ending(invitation.expires_at)}</span>{" "}
                <Confirm label="Revoke" question={`Revoke the invitation to ${invitation.email}?`} busy={changing.busy} confirm={() => act({ kind: "revoke", member: invitation.member })} />
              </li>
            ))}
          </ul>
        </>
      )}
      {manages(role) && <Invite owner={role === "owner"} busy={changing.busy} invite={(email, as, done) => act({ kind: "invite", email, role: as }, done)} />}
      {changing.problem !== undefined && <p role="alert">{changing.problem}</p>}
      {role !== "owner" && (
        <Confirm label="Leave" question={`Leave ${workspace.name}?`} busy={changing.busy} confirm={() => act({ kind: "leave" }, () => open.left(`You left ${workspace.name}.`))} />
      )}
    </section>
  );
}

function Member({
  member,
  me,
  by,
  busy,
  act,
}: {
  member: MemberSummary;
  me: boolean;
  by: WorkspaceRole;
  busy: boolean;
  act(action: MemberAction): void;
}) {
  const { id, name, role } = member;
  const changes = !me && by === "owner" && mayManage(by, role);
  return (
    <li>
      {name}
      {me && " (you)"} <span className="muted">{role === undefined ? "Past member" : roleText(role)}</span>
      {changes && (
        <button type="button" disabled={busy} onClick={() => act({ kind: "set_role", member: id, role: role === "admin" ? "member" : "admin" })}>
          {role === "admin" ? "Make member" : "Make admin"}
        </button>
      )}
      {!me && mayManage(by, role) && <Confirm label="Remove" question={`Remove ${name} from this workspace?`} busy={busy} confirm={() => act({ kind: "remove", member: id })} />}
    </li>
  );
}

function Invite({ owner, busy, invite }: { owner: boolean; busy: boolean; invite(email: string, role: WorkspaceRole, done: () => void): void }) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<WorkspaceRole>("member");
  const [sent, setSent] = useState<string>();
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        setSent(undefined);
        invite(email.trim(), role, () => {
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
      {owner && (
        <label>
          Role
          <select value={role} onChange={(event) => setRole(event.target.value === "admin" ? "admin" : "member")}>
            <option value="member">Member</option>
            <option value="admin">Admin</option>
          </select>
        </label>
      )}
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
