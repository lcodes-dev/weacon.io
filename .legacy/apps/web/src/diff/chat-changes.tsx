import { type ReactNode, useState } from "react";

import type { Send } from "../composer/prompt.ts";
import type { MachineId } from "../protocol.ts";
import { useSend } from "../composer/send.ts";
import type { Changes } from "./changes.ts";
import { type Comment, unsent } from "./comments.ts";
import { DiffView } from "./diff-view.tsx";
import { reviewPrompt } from "./review-prompt.ts";

/**
 * The changes a chat's agent made, with the comments on them, under a `bar` such as its worktree's, kept while `hidden`; who may call
 * that agent, on `machine`, can send them to it as one prompt, unless `blocked` says why not. A comment edited while its
 * review was on its way stays.
 */
export function ChatChanges({
  changes,
  hidden,
  machine,
  send,
  blocked,
  bar,
}: {
  changes: Changes;
  hidden: boolean;
  machine: MachineId | undefined;
  send: Send;
  blocked: string | undefined;
  bar?: ReactNode;
}) {
  const [comments, setComments] = useState<readonly Comment[]>([]);
  const { sending, problem, submit } = useSend<readonly Comment[]>(send, (sent) => setComments((now) => unsent(now, sent)));
  // A review goes to the agent whose changes it is of.
  const sendReview = () => submit(reviewPrompt(comments), comments, machine);
  if (hidden) {
    return null;
  }
  return (
    <div className="pane chat-changes">
      {bar}
      {machine !== undefined && comments.length > 0 && (
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
