/** Lines picked in one file: rows `anchor` to `focus` of its rows, either way round. */
export type Selection = { path: string; anchor: number; focus: number };

/** A comment on rows `first` to `last` of a file's rows, which were lines `where` when it was made. */
export type Comment = { id: number; path: string; first: number; last: number; where: string; text: string };

/** The first and last rows `selection` covers. */
export function span({ anchor, focus }: Selection): { first: number; last: number } {
  return { first: Math.min(anchor, focus), last: Math.max(anchor, focus) };
}

/** The selection after a person picks `row` of `path`, extending one in the same file when `extend`. */
export function selectRows(selection: Selection | undefined, path: string, row: number, extend: boolean): Selection {
  return extend && selection?.path === path ? { ...selection, focus: row } : { path, anchor: row, focus: row };
}

/** `comments` with one saying `text` on `selection`, the lines `where`, after the others. */
export function addComment(comments: readonly Comment[], selection: Selection, where: string, text: string): readonly Comment[] {
  const id = Math.max(0, ...comments.map((comment) => comment.id)) + 1;
  return [...comments, { id, path: selection.path, ...span(selection), where, text }];
}

/** `comments` with comment `id` saying `text` instead. */
export function editComment(comments: readonly Comment[], id: number, text: string): readonly Comment[] {
  return comments.map((comment) => (comment.id === id ? { ...comment, text } : comment));
}

/**
 * The comments of `now` a review that sent `sent` leaves: those it didn't carry. An edited comment
 * is a new one, as is one made again under a removed one's id.
 */
export function unsent(now: readonly Comment[], sent: readonly Comment[]): readonly Comment[] {
  return now.filter((comment) => !sent.includes(comment));
}

export function removeComment(comments: readonly Comment[], id: number): readonly Comment[] {
  return comments.filter((comment) => comment.id !== id);
}
