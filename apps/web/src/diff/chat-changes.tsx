import { type ReactNode, useRef, useState } from "react";

import type { Lates, Send } from "../composer/prompt.ts";
import { useSend } from "../composer/send.ts";
import type { Changes } from "./changes.ts";
import { type Comment, unsent } from "./comments.ts";
import { DiffView } from "./diff-view.tsx";
import { reviewPrompt } from "./review-prompt.ts";

/**
 * A chat's changes with the comments on them, under a `bar` such as its worktree's, kept while `hidden`; its owner (`mine`) can send
 * them to the agent as one prompt, unless `blocked` says why not. A comment edited while its
 * review was on its way stays.
 */
export function ChatChanges({
  changes,
  hidden,
  mine,
  send,
  lates,
  blocked,
  bar,
}: {
  changes: Changes;
  hidden: boolean;
  mine: boolean;
  send: Send;
  /** Late receipts, among them a review's. */
  lates: Lates;
  blocked: string | undefined;
  bar?: ReactNode;
}) {
  const [comments, setComments] = useState<readonly Comment[]>([]);
  // The comments each review sent carried, by its text.
  const carried = useRef(new Map<string, readonly Comment[]>());
  const { sending, problem, submit } = useSend(send, lates, (text) => {
    const sent = carried.current.get(text) ?? [];
    setComments((now) => unsent(now, sent));
  });
  const sendReview = async () => {
    const text = reviewPrompt(comments);
    carried.current.set(text, comments);
    await submit(text);
  };
  if (hidden) {
    return null;
  }
  return (
    <div className="pane chat-changes">
      {bar}
      {mine && comments.length > 0 && (
        <div className="review-send">
          <button type="button" disabled={sending || blocked !== undefined} onClick={() => void sendReview()}>
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
