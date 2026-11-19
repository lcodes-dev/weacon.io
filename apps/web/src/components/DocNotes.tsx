import { useMutation, useQueryClient } from "@tanstack/react-query"
import type { ReactNode } from "react"
import { api, type DocsPage, type Note } from "../api"

/** The docs writer's notes on a person's words: what was written, and what the code does instead. `actions` draws a note's buttons. */
export function DocNotes({ notes, actions }: { notes: Note[]; actions?: (note: Note, place: number) => ReactNode }) {
  return notes.map((note, place) => (
    // A note has no name but its place, which is also how the server knows it.
    <div key={place} className="flex flex-col gap-1 rounded-lg border border-amb-line bg-amb-soft px-3 py-2">
      <span className="lbl text-amb">The writer reads the code differently</span>
      <q className="italic">{note.quote}</q>
      <p>
        {note.says} {note.place && <code className="font-mono text-[12px] text-ink2">{note.place}</code>}
      </p>
      {note.instead && <p className="text-ink2">It suggests: {note.instead}</p>}
      {actions && <div className="flex gap-2 pt-1">{actions(note, place)}</div>}
    </div>
  ))
}

/** The notes on a page as it is, each with its answers. `path` is the project's API path. */
export function PageNotes({ path, page }: { path: string; page: DocsPage }) {
  const queryClient = useQueryClient()
  const changed = { onSuccess: () => queryClient.invalidateQueries({ queryKey: ["docs"] }) }
  const dismiss = useMutation({ mutationFn: (place: number) => api("DELETE", `${path}/docs/pages/${page.id}/notes/${place}`), ...changed })
  // Puts the sentence the writer suggests in place of the person's, as an edit of theirs.
  const use = useMutation({
    mutationFn: ({ quote, instead }: Note) =>
      api("PUT", `${path}/docs/pages/${page.id}`, { title: page.title, markdown: page.markdown.replace(quote, instead!), version: page.version }),
    ...changed,
  })
  const error = dismiss.error ?? use.error
  return (
    <>
      {page.checking && <p className="text-ink2">Checking against the code…</p>}
      <DocNotes
        notes={page.notes ?? []}
        actions={(note, place) => (
          <>
            {note.instead && page.markdown.includes(note.quote) && (
              <button className="btn btn-sm bg-bg" disabled={use.isPending} onClick={() => use.mutate(note)}>
                Use the suggestion
              </button>
            )}
            <button className="btn btn-sm bg-bg" disabled={dismiss.isPending} onClick={() => dismiss.mutate(place)}>
              Dismiss
            </button>
          </>
        )}
      />
      {error && <p className="text-acc">{error.message}</p>}
    </>
  )
}
