import { useRef } from "react";

import { type Attachment, size } from "./attachments.ts";

/** Opens the file picker, and hands over the files chosen. */
export function AttachButton({ attach }: { attach: (files: File[]) => void }) {
  const input = useRef<HTMLInputElement>(null);
  return (
    <>
      <button type="button" onClick={() => input.current?.click()}>
        Attach
      </button>
      <input
        ref={input}
        type="file"
        multiple
        hidden
        onChange={(event) => {
          attach([...(event.target.files ?? [])]);
          // The same file can be chosen again after it is removed.
          event.target.value = "";
        }}
      />
    </>
  );
}

/** The files added to a message, each with its size and a way to remove it. */
export function Files({ attachments, remove }: { attachments: readonly Attachment[]; remove: (id: number) => void }) {
  return (
    <ul className="files" aria-label="Attached files">
      {attachments.map(({ id, file }) => (
        <li key={id}>
          <span className="name">{file.name}</span>
          <span className="muted">{size(file.size)}</span>
          <button type="button" aria-label={`Remove ${file.name}`} onClick={() => remove(id)}>
            ×
          </button>
        </li>
      ))}
    </ul>
  );
}
