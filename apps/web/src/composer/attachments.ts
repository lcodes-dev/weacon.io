/** A file added to the message box, shown before sending. */
export type Attachment = { id: number; file: File };

export const MAX_FILES = 10;
export const MAX_BYTES = 10 * 1024 * 1024;

/** A message's files, why any were left out, and the id the next one takes. */
export type Attached = { attachments: Attachment[]; refused: string | undefined; next: number };

export const NONE: Attached = { attachments: [], refused: undefined, next: 1 };

/** Adds `files`. Empty files, files over `MAX_BYTES`, and files beyond `MAX_FILES` are left out. */
export function add({ attachments, next }: Attached, files: readonly File[]): Attached {
  const fit = files.filter((file) => file.size > 0 && file.size <= MAX_BYTES);
  const room = MAX_FILES - attachments.length;
  const added = fit.slice(0, room).map((file, at) => ({ id: next + at, file }));
  return {
    attachments: [...attachments, ...added],
    refused: refusal(files.length - fit.length, fit.length - added.length),
    next: next + added.length,
  };
}

/** The files once the person has moved on: why any were left out no longer shows. */
export function settled(attached: Attached): Attached {
  return attached.refused === undefined ? attached : { ...attached, refused: undefined };
}

/** The files without the one numbered `id`; what was left out before no longer matters. */
export function remove({ attachments, next }: Attached, id: number): Attached {
  return { attachments: attachments.filter((attachment) => attachment.id !== id), refused: undefined, next };
}

function refusal(unfit: number, extra: number): string | undefined {
  if (extra > 0) {
    return `A message can carry ${MAX_FILES} files`;
  }
  return unfit > 0 ? `Files must be ${size(MAX_BYTES)} or smaller, and not empty` : undefined;
}

/** `bytes` for people: B, KB or MB, in units of 1,024. */
export function size(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const kb = bytes / 1024;
  return kb < 1024 ? `${Math.round(kb)} KB` : `${Math.round((kb / 1024) * 10) / 10} MB`;
}
