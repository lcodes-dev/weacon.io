import type { useWorktree } from "./use-worktree.ts";
import { busy, removeQuestion, stateLine } from "./worktree.ts";
import "./worktree.css";

/**
 * Where the chat's worktree stands, with what its owner can do with it: open a pull request,
 * remove it after saying what goes, and open a terminal on its machine,
 * or carry the chat on by hand with the line it shows.
 */
export function ReviewBar({ bar, title }: { bar: ReturnType<typeof useWorktree>; title: string }) {
  const { worktree, draft } = bar;
  const drafting = draft !== undefined;
  const { standing, confirming, outcome } = worktree;
  const waiting = busy(worktree);
  const present = standing.kind === "state" && !standing.removed;
  return (
    <section className="review-bar" aria-label="Worktree">
      {/* Its buttons share the state's row, so the diff below doesn't move as they come. */}
      <div className="standing">
        <p className="muted">{stateLine(standing)}</p>
        {confirming === undefined && !drafting && (
          <div className="actions">
            {present && (
              <>
                <button type="button" disabled={waiting} onClick={() => bar.editDraft({ title, body: "" })}>
                  Open pull request
                </button>
                <button type="button" disabled={waiting} onClick={bar.remove}>
                  Remove
                </button>
              </>
            )}
            {standing.kind === "unknown" && outcome?.kind === "problem" && !waiting && (
            <button type="button" onClick={bar.refresh}>
              Try again
            </button>
          )}
          {standing.kind !== "unknown" && (
              <button type="button" disabled={waiting} onClick={bar.openTerminal}>
                Open terminal
              </button>
            )}
          </div>
        )}
      </div>
      {confirming !== undefined && (
        <div className="confirm" role="group" aria-label="Remove the worktree">
          <p>{removeQuestion(confirming)}</p>
          <button type="button" onClick={bar.confirmRemove}>
            Remove
          </button>
          <button type="button" onClick={bar.cancel}>
            Cancel
          </button>
        </div>
      )}
      {draft !== undefined && (
        <PullRequestForm draft={draft} edit={bar.editDraft} waiting={waiting} open={bar.openPullRequest} cancel={() => bar.editDraft(undefined)} />
      )}
      {bar.resume !== undefined && standing.kind !== "unknown" && (
        <p className="muted">
          To carry this chat on in a terminal on its machine, run <code>{bar.resume}</code>
        </p>
      )}
      {outcome?.kind === "link" && (
        <p>
          {outcome.text}{" "}
          <a href={outcome.url} target="_blank" rel="noreferrer">
            Open it
          </a>
        </p>
      )}
      {outcome?.kind === "note" && <p>{outcome.text}</p>}
      {outcome?.kind === "problem" && <p role="alert">{outcome.text}</p>}
    </section>
  );
}

// The draft stays, as typed, until the core opens the pull request or the person cancels.
function PullRequestForm({
  draft,
  edit,
  waiting,
  open,
  cancel,
}: {
  draft: { title: string; body: string };
  edit: (draft: { title: string; body: string }) => void;
  waiting: boolean;
  open: () => void;
  cancel: () => void;
}) {
  return (
    <form
      className="pull-request"
      onSubmit={(event) => {
        event.preventDefault();
        open();
      }}
    >
      <label>
        Title
        <input value={draft.title} disabled={waiting} onChange={(event) => edit({ ...draft, title: event.target.value })} />
      </label>
      <label>
        Description
        <textarea value={draft.body} disabled={waiting} onChange={(event) => edit({ ...draft, body: event.target.value })} />
      </label>
      <button type="submit" disabled={waiting || draft.title.trim() === ""}>
        Open draft pull request
      </button>
      <button type="button" onClick={cancel}>
        Cancel
      </button>
    </form>
  );
}
