import { useState } from "react";

import type { useWorktree } from "./use-worktree.ts";
import { busy, removeQuestion, stateLine } from "./worktree.ts";
import "./worktree.css";

/**
 * Where the chat's worktree stands, with what its owner can do with it: open a pull request,
 * remove it to the Trash after saying what goes, restore it, and open a terminal where the core can.
 */
export function ReviewBar({ bar, title }: { bar: ReturnType<typeof useWorktree>; title: string }) {
  const [drafting, setDrafting] = useState(false);
  const { worktree, terminals } = bar;
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
                <button type="button" disabled={waiting} onClick={() => setDrafting(true)}>
                  Open pull request
                </button>
                <button type="button" disabled={waiting} onClick={bar.remove}>
                  Remove
                </button>
              </>
            )}
            {standing.kind === "state" && standing.removed && (
              <button type="button" disabled={waiting} onClick={bar.restore}>
                Restore
              </button>
            )}
            {standing.kind === "unknown" && outcome?.kind === "problem" && !waiting && (
            <button type="button" onClick={bar.refresh}>
              Try again
            </button>
          )}
          {terminals && standing.kind !== "unknown" && (
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
      {drafting && (
        <PullRequestForm
          title={title}
          open={(chosen, body) => {
            setDrafting(false);
            bar.openPullRequest(chosen, body);
          }}
          cancel={() => setDrafting(false)}
        />
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

function PullRequestForm({ title, open, cancel }: { title: string; open: (title: string, body: string) => void; cancel: () => void }) {
  const [chosen, setChosen] = useState(title);
  const [body, setBody] = useState("");
  return (
    <form
      className="pull-request"
      onSubmit={(event) => {
        event.preventDefault();
        open(chosen.trim(), body);
      }}
    >
      <label>
        Title
        <input value={chosen} onChange={(event) => setChosen(event.target.value)} />
      </label>
      <label>
        Description
        <textarea value={body} onChange={(event) => setBody(event.target.value)} />
      </label>
      <button type="submit" disabled={chosen.trim() === ""}>
        Open draft pull request
      </button>
      <button type="button" onClick={cancel}>
        Cancel
      </button>
    </form>
  );
}
