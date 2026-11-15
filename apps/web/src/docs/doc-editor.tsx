import { type ReactNode, useState } from "react";

import type { DocContent } from "../protocol.ts";

/** A doc's title and Markdown to edit from `initial`, with Save and Cancel; `children` show above the buttons. */
export function DocEditor({
  initial,
  busy,
  problem,
  save,
  cancel,
  edited,
  children,
}: {
  initial: DocContent;
  busy: boolean;
  problem: string | undefined;
  save(content: DocContent): void;
  cancel(): void;
  /** Told of each change, so the page holding the edit keeps it. */
  edited?: (content: DocContent) => void;
  children?: ReactNode;
}) {
  const [content, setContent] = useState(initial);
  const change = (next: DocContent) => {
    setContent(next);
    edited?.(next);
  };
  return (
    <form
      className="doc-editor"
      onSubmit={(event) => {
        event.preventDefault();
        save({ ...content, title: content.title.trim() });
      }}
    >
      <label>
        Title
        <input aria-label="Title" required value={content.title} onChange={(event) => change({ ...content, title: event.target.value })} />
      </label>
      <label>
        Text
        <textarea aria-label="Text" rows={20} value={content.body} onChange={(event) => change({ ...content, body: event.target.value })} />
      </label>
      {problem !== undefined && <p role="alert">{problem}</p>}
      {children}
      <div className="actions">
        <button type="submit" disabled={busy}>
          Save
        </button>
        <button type="button" onClick={cancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
