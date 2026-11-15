import type { Names } from "../chat/label.ts";
import type { ProposalOutcome, ProposalRecord, TaskEditBody, TaskRecord } from "../protocol.ts";

/** What a proposal is about, in a line: "Edit of the doc “Plan”", or "New task “Ship it”". */
export function aboutText(proposal: ProposalRecord, tasks: readonly TaskRecord[]): string {
  const { about, change } = proposal;
  const title = (id: string) => tasks.find((task) => task.id === id)?.title;
  switch (about.kind) {
    case "doc":
      return change?.kind === "doc_edit" ? `Edit of the doc “${change.content.title}”` : "Edit of a doc";
    case "task": {
      const named = title(about.task);
      return named === undefined ? "Edit of a task" : `Edit of the task “${named}”`;
    }
    case "new_doc":
      return change?.kind === "new_doc" ? `New doc “${change.content.title}”` : "New doc";
    case "new_task":
      return change?.kind === "new_task" ? `New task “${change.title}”` : (title(about.task) ?? "New task");
    default:
      return "A change this app can't show";
  }
}

/** Each edit a task edit makes, in a line. */
export function taskEditText(edit: TaskEditBody, tasks: readonly TaskRecord[]): string {
  switch (edit.kind) {
    case "title":
      return `Retitle it “${edit.title}”`;
    case "status":
      return `Mark it ${edit.status === "todo" ? "to do" : edit.status === "in_progress" ? "in progress" : "done"}`;
    case "move": {
      const before = tasks.find((task) => task.id === edit.before)?.title;
      return edit.before === undefined ? "Move it last" : before === undefined ? "Move it" : `Move it before “${before}”`;
    }
    default:
      return "An edit this app can't show";
  }
}

/** Where a decided proposal ended, such as "Accepted by ada"; nothing for an open one. */
export function stateText(proposal: ProposalRecord, names: Names): string | undefined {
  const who = (member: string) => names.get(member) ?? "a member";
  switch (proposal.state) {
    case "open":
      return undefined;
    case "accepted":
      return `Accepted by ${who(proposal.by)}`;
    case "rejected":
      return `Rejected by ${who(proposal.by)}`;
    case "superseded":
      return "Replaced by a newer edit";
    case "withdrawn":
      return "Withdrawn";
    default:
      return "Closed";
  }
}

/** What to tell the person once their decision came to `outcome` and the proposal stayed open. */
export function conflictText(outcome: ProposalOutcome): string | undefined {
  switch (outcome.outcome) {
    case "doc_conflict":
      return "The doc changed where this edit changes it too, so nothing was changed. Edit the doc yourself, then reject this proposal.";
    case "task_conflict":
      return "The task changed where this edit changes it too, so nothing was changed. Edit the task yourself, then reject this proposal.";
    default:
      return undefined;
  }
}
