import { type DragEvent as ReactDragEvent, useEffect, useRef, useState } from "react";

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

/**
 * Takes files dropped on the element its props are spread on, and marks it while they are dragged
 * over it. Files dropped anywhere else are kept from opening in place of the app.
 */
export function useFileDrop(attach: (files: File[]) => void) {
  const [dropping, setDropping] = useState(false);
  useEffect(() => {
    // A file dropped beside the box would otherwise open in place of the app, losing the draft.
    const keep = (event: DragEvent) => {
      if (event.dataTransfer?.types.includes("Files")) {
        event.preventDefault();
      }
    };
    document.addEventListener("dragover", keep);
    document.addEventListener("drop", keep);
    return () => {
      document.removeEventListener("dragover", keep);
      document.removeEventListener("drop", keep);
    };
  }, []);
  return {
    "data-dropping": dropping || undefined,
    onDragOver(event: ReactDragEvent<HTMLElement>) {
      if (event.dataTransfer.types.includes("Files")) {
        event.preventDefault();
        setDropping(true);
      }
    },
    onDragLeave(event: ReactDragEvent<HTMLElement>) {
      if (!event.currentTarget.contains(event.relatedTarget as Node | null)) {
        setDropping(false);
      }
    },
    onDrop(event: ReactDragEvent<HTMLElement>) {
      setDropping(false);
      // Dropped text goes into the box as usual.
      if (event.dataTransfer.types.includes("Files")) {
        event.preventDefault();
        attach(dropped(event.dataTransfer));
      }
    },
  };
}

// The files dropped, without folders.
function dropped(data: DataTransfer): File[] {
  return [...data.items].flatMap((item) => {
    const file = item.kind === "file" && item.webkitGetAsEntry()?.isDirectory !== true ? item.getAsFile() : null;
    return file === null ? [] : [file];
  });
}
