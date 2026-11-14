/** Lines picked in one file: rows `anchor` to `focus` of its rows, either way round. */
export type Selection = { path: string; anchor: number; focus: number };

/** A comment on rows `first` to `last` of a file's rows. */
export type Comment = { id: number; path: string; first: number; last: number; text: string };

/** The first and last rows `selection` covers. */
export function span({ anchor, focus }: Selection): { first: number; last: number } {
  return { first: Math.min(anchor, focus), last: Math.max(anchor, focus) };
}

/** The selection after a person picks `row` of `path`, extending one in the same file when `extend`. */
export function selectRows(selection: Selection | undefined, path: string, row: number, extend: boolean): Selection {
  return extend && selection?.path === path ? { ...selection, focus: row } : { path, anchor: row, focus: row };
}

/** `comments` with one saying `text` on `selection`, after the others. */
export function addComment(comments: readonly Comment[], selection: Selection, text: string): readonly Comment[] {
  const id = Math.max(0, ...comments.map((comment) => comment.id)) + 1;
  return [...comments, { id, path: selection.path, ...span(selection), text }];
}

/** `comments` with comment `id` saying `text` instead. */
export function editComment(comments: readonly Comment[], id: number, text: string): readonly Comment[] {
  return comments.map((comment) => (comment.id === id ? { ...comment, text } : comment));
}

export function removeComment(comments: readonly Comment[], id: number): readonly Comment[] {
  return comments.filter((comment) => comment.id !== id);
}
