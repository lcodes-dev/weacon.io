import { useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { type Loading, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { type Names, authorText } from "../members/names.ts";
import type { DocAnchors, ProposalChange, ProposalList, ProposalOutcome, ProposalRecord, ProposalVerdict, TaskRecord, WorkspaceId } from "../protocol.ts";
import { useReloadOn } from "../shell/feed-hooks.ts";
import type { WorkspaceSocket } from "../shell/workspace-socket.ts";
import { DiffLines } from "../diff/diff-lines.tsx";
import "./proposals.css";
import { aboutText, conflictText, pinChanges, stateText, taskEditText } from "./proposals.ts";

type Props = { api: Api; workspace: WorkspaceId; socket: WorkspaceSocket; names: Names };

/** The changes agents propose to the workspace's docs and tasks, open ones to accept or reject, kept fresh by its feed. */
export function ProposalsPage({ api, workspace, socket, names }: Props) {
  const proposals = useLoaded(() => api.proposals(workspace), workspace);
  const tasks = useLoaded(() => api.tasks(workspace), workspace);
  useReloadOn(socket, ({ kind }) => kind === "proposal_changed", () => void proposals.load());
  useReloadOn(socket, ({ kind }) => kind === "task_changed", () => void tasks.load());
  const list = proposals.value?.proposals;
  const open = list?.filter(({ state }) => state === "open") ?? [];
  const decided = list?.filter(({ state }) => state !== "open") ?? [];
  const card = (proposal: ProposalRecord) => (
    <Card key={proposal.id} api={api} workspace={workspace} names={names} proposal={proposal} tasks={tasks.value?.tasks ?? []} proposals={proposals} />
  );
  return (
    <div className="page proposals">
      {proposals.loading && <p className="muted">Loading</p>}
      {list !== undefined && open.length === 0 && <p className="muted">No open proposals. When an agent proposes a change to a doc or task, it waits here for someone to accept it.</p>}
      {open.length > 0 && <ul aria-label="Open proposals">{open.map(card)}</ul>}
      {decided.length > 0 && (
        <>
          <h2>Decided</h2>
          <ul aria-label="Decided proposals">{decided.map(card)}</ul>
        </>
      )}
      {proposals.problem !== undefined && <Problem text={proposals.problem} retry={() => void proposals.load()} />}
      {tasks.problem !== undefined && <Problem text={tasks.problem} retry={() => void tasks.load()} />}
    </div>
  );
}

type CardProps = { api: Api; workspace: WorkspaceId; names: Names; proposal: ProposalRecord; tasks: readonly TaskRecord[]; proposals: Loading<ProposalList> };

// One proposal: what it is about, who proposed it, and while it is open its changes, Accept and Reject.
function Card({ api, workspace, names, proposal, tasks, proposals }: CardProps) {
  const deciding = useAsk();
  const [conflict, setConflict] = useState<ProposalOutcome>();
  const about = aboutText(proposal, tasks);
  const decide = (verdict: ProposalVerdict) => {
    setConflict(undefined);
    void deciding.ask(
      () => api.decide(workspace, proposal.id, verdict),
      (outcome) => (conflictText(outcome) === undefined ? void proposals.load() : setConflict(outcome)),
      () => void proposals.load(),
    );
  };
  const state = stateText(proposal, names);
  return (
    <li>
      <article aria-label={about}>
        <h3>{about}</h3>
        <p className="muted">
          Proposed by {authorText(proposal.author, names)}
          {state !== undefined && ` · ${state}`}
        </p>
        {proposal.change !== undefined && <Changes api={api} workspace={workspace} change={proposal.change} tasks={tasks} />}
        {conflict !== undefined && <Conflict outcome={conflict} />}
        {proposal.state === "open" && (
          <div className="actions">
            <button type="button" disabled={deciding.busy} onClick={() => decide("accept")}>
              Accept
            </button>
            <button type="button" disabled={deciding.busy} onClick={() => decide("reject")}>
              Reject
            </button>
          </div>
        )}
        {deciding.problem !== undefined && <p role="alert">{deciding.problem}</p>}
      </article>
    </li>
  );
}

function Changes({ api, workspace, change, tasks }: { api: Api; workspace: WorkspaceId; change: ProposalChange; tasks: readonly TaskRecord[] }) {
  switch (change.kind) {
    case "doc_edit":
      return <DocEdit api={api} workspace={workspace} change={change} />;
    case "task_edit":
      return (
        <ul className="edits">
          {change.edits.map((edit, at) => (
            <li key={at}>{taskEditText(edit, tasks)}</li>
          ))}
        </ul>
      );
    case "new_doc":
      return (
        <>
          <DiffLines before="" after={change.content.body} label="Its text" />
          <Pins before={undefined} after={change.content.anchors} />
        </>
      );
    default:
      return null;
  }
}

// A doc edit, compared with the revision it was written from.
function DocEdit({ api, workspace, change }: { api: Api; workspace: WorkspaceId; change: Extract<ProposalChange, { kind: "doc_edit" }> }) {
  const base = useLoaded(() => api.docRevision(workspace, change.doc, change.base), `${workspace} ${change.doc} ${change.base}`);
  const before = base.value?.content;
  return (
    <>
      {base.loading && <p className="muted">Loading</p>}
      {base.problem !== undefined && <Problem text={base.problem} retry={() => void base.load()} />}
      {before !== undefined && before.title !== change.content.title && <p>Retitle it “{change.content.title}”</p>}
      {before !== undefined && <DiffLines before={before.body} after={change.content.body} label="Its changes" />}
      {before !== undefined && <Pins before={before.anchors} after={change.content.anchors} />}
    </>
  );
}

// The links to code a doc change pins anew or drops, which its text alone may not show.
function Pins({ before, after }: { before: DocAnchors | undefined; after: DocAnchors | undefined }) {
  const { added, removed } = pinChanges(before, after);
  if (added.length === 0 && removed.length === 0) {
    return null;
  }
  return (
    <ul className="edits" aria-label="Its links to code">
      {added.map((pin) => (
        <li key={`+${pin}`}>
          Pins <code>{pin}</code>
        </li>
      ))}
      {removed.map((pin) => (
        <li key={`-${pin}`}>
          Drops <code>{pin}</code>
        </li>
      ))}
    </ul>
  );
}

// Why accepting changed nothing, and for a doc, where the two differ.
function Conflict({ outcome }: { outcome: ProposalOutcome }) {
  const hunks = outcome.outcome === "doc_conflict" ? outcome.hunks : [];
  return (
    <>
      <p role="alert">{conflictText(outcome)}</p>
      {hunks.map((hunk, at) => (hunk.kind === "conflict" ? <DiffLines key={at} before={hunk.current} after={hunk.proposed} label="The doc now and this edit" /> : null))}
    </>
  );
}
