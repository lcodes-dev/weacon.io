import { useRef, useState } from "react";

import { type Retry, type Send, after, requestFor } from "../composer/prompt.ts";
import { newRequestId } from "../ids.ts";
import type { Changes } from "./changes.ts";
import type { Comment } from "./comments.ts";
import { DiffView } from "./diff-view.tsx";
import { reviewPrompt } from "./review-prompt.ts";

/**
 * A chat's changes with the comments on them, kept while `hidden`; its owner (`mine`) can send
 * them to the agent as one prompt, unless `blocked` says why not.
 */
export function ChatChanges({
  changes,
  hidden,
  mine,
  send,
  blocked,
}: {
  changes: Changes;
  hidden: boolean;
  mine: boolean;
  send: Send;
  blocked: string | undefined;
}) {
  const [comments, setComments] = useState<readonly Comment[]>([]);
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  // The request the last review was sent under, so sending the same review again can't run it twice.
  const retry = useRef<Retry>(undefined);
  if (hidden) {
    return null;
  }
  const sendReview = () => {
    const text = reviewPrompt(changes.files, comments);
    const request = requestFor(retry.current, text, newRequestId);
    const sent = new Set(comments.map(({ id }) => id));
    setSending(true);
    setProblem(undefined);
    void send(text, request).then((ended) => {
      const next = after(ended, text, request);
      retry.current = next.retry;
      setSending(false);
      setProblem(next.problem);
      if (ended === "accepted") {
        // Comments made while it was on its way stay for the next review.
        setComments((now) => now.filter(({ id }) => !sent.has(id)));
      }
    });
  };
  return (
    <div className="chat-changes">
      {mine && comments.length > 0 && (
        <div className="review-send">
          <button type="button" disabled={sending || blocked !== undefined} onClick={sendReview}>
            Send review
          </button>
          {blocked !== undefined && <span className="muted">{blocked}</span>}
          {problem !== undefined && <span role="alert">{problem}</span>}
        </div>
      )}
      <DiffView changes={changes} comments={comments} setComments={setComments} />
    </div>
  );
}
