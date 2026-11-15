import type { ReactNode } from "react";

import type { DocContent } from "../protocol.ts";

/**
 * A doc's title and Markdown, which `change` is told of, with Save and Cancel; `children` show
 * above the buttons. While `busy` saving, nothing changes, so what was sent is what is shown.
 */
export function DocEditor({
  content,
  change,
  busy,
  problem,
  save,
  cancel,
  children,
}: {
  content: DocContent;
  change(content: DocContent): void;
  busy: boolean;
  problem: string | undefined;
  save(content: DocContent): void;
  cancel(): void;
  children?: ReactNode;
}) {
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
        <input aria-label="Title" required readOnly={busy} value={content.title} onChange={(event) => change({ ...content, title: event.target.value })} />
      </label>
      <label>
        Text
        <textarea aria-label="Text" rows={20} readOnly={busy} value={content.body} onChange={(event) => change({ ...content, body: event.target.value })} />
      </label>
      {problem !== undefined && <p role="alert">{problem}</p>}
      {children}
      <div className="actions">
        <button type="submit" disabled={busy}>
          Save
        </button>
        <button type="button" disabled={busy} onClick={cancel}>
          Cancel
        </button>
      </div>
    </form>
  );
}
