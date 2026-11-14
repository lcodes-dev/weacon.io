import { useState } from "react";

import type { Comment } from "./comments.ts";

/** A box for a comment's `text`, with `action` to keep it and Cancel; Escape cancels too. */
export function Composer({
  label,
  text,
  action,
  onChange,
  onSave,
  onCancel,
}: {
  label: string;
  text: string;
  action: string;
  onChange: (text: string) => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  return (
    <div className="comment">
      <p className="muted">{label}</p>
      <textarea
        aria-label={label}
        value={text}
        autoFocus
        onChange={(event) => onChange(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Escape") {
            onCancel();
          }
        }}
      />
      <div className="actions">
        <button type="button" disabled={text.trim() === ""} onClick={onSave}>
          {action}
        </button>
        <button type="button" onClick={onCancel}>
          Cancel
        </button>
      </div>
    </div>
  );
}

/** A comment under the lines it is on, which its reader can edit or remove. */
export function CommentView({
  comment,
  label,
  onEdit,
  onRemove,
}: {
  comment: Comment;
  label: string;
  onEdit: (text: string) => void;
  onRemove: () => void;
}) {
  const [editing, setEditing] = useState<string>();
  if (editing !== undefined) {
    return (
      <Composer
        label={label}
        text={editing}
        action="Save"
        onChange={setEditing}
        onSave={() => {
          onEdit(editing);
          setEditing(undefined);
        }}
        onCancel={() => setEditing(undefined)}
      />
    );
  }
  return (
    <div className="comment">
      <p className="muted">{label}</p>
      <p className="text">{comment.text}</p>
      <div className="actions">
        <button type="button" onClick={() => setEditing(comment.text)}>
          Edit
        </button>
        <button type="button" onClick={onRemove}>
          Remove
        </button>
      </div>
    </div>
  );
}
