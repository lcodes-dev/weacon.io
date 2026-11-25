import { useMutation, useQueryClient } from "@tanstack/react-query"
import { diffArrays } from "diff"
import { useState } from "react"
import { Link } from "react-router"
import { api, type DocsPage, type Proposal, who } from "../api"
import { DocNotes } from "./DocNotes"

const blocks = (text: string) => (text.trim() ? text.trim().split(/\n{2,}/) : [])

/** A change the docs writer or an agent suggests, shown block by block against the page as it is. `page` is the page it changes, and `path` the project's API path. `onDone` is told what was rejected. */
export function DocProposal(props: { path: string; proposal: Proposal; page?: DocsPage; onDone: (rejected?: Proposal) => void }) {
  const { path, proposal, page } = props
  const queryClient = useQueryClient()
  // The changes left out, by their place.
  const [left, setLeft] = useState<number[]>([])
  const done = (rejected?: Proposal) => queryClient.invalidateQueries({ queryKey: ["docs"] }).then(() => props.onDone(rejected))
  const accept = useMutation({
    // The server refuses when the page is no longer at the version shown here.
    mutationFn: (markdown?: string) => api("POST", `${path}/docs/proposals/${proposal.id}`, { markdown, version: page?.version }),
    onSuccess: () => done(),
    onError: () => queryClient.invalidateQueries({ queryKey: ["docs"] }),
  })
  const reject = useMutation({
    mutationFn: () => api("DELETE", `${path}/docs/proposals/${proposal.id}`),
    onSuccess: () => done(proposal),
  })
  // Each change is the blocks it takes out and the blocks it puts in; between changes are the blocks that stay.
  const changes: { same?: string[]; removed: string[]; added: string[] }[] = []
  for (const part of diffArrays(blocks(page?.markdown ?? ""), blocks(proposal.markdown ?? ""))) {
    const last = changes.at(-1)
    if (!part.added && !part.removed) changes.push({ same: part.value, removed: [], added: [] })
    else if (last && !last.same) (part.added ? last.added : last.removed).push(...part.value)
    else changes.push({ removed: part.removed ? part.value : [], added: part.added ? part.value : [] })
  }
  const chosen = changes.flatMap((change, place) => change.same ?? (left.includes(place) ? change.removed : change.added)).join("\n\n")
  const about = (change: (typeof changes)[number]) =>
    proposal.notes.filter((note) => [...change.removed, ...change.added, ...(change.same ?? [])].some((block) => block.includes(note.quote)))
  const placed = changes.flatMap(about)
  const kind = !proposal.page ? "A new page" : proposal.markdown === null ? "Remove the page" : "A change to"
  const error = accept.error ?? reject.error
  return (
    <div className="flex max-w-[720px] flex-col gap-3 pt-4">
      <div className="flex flex-col gap-1">
        <span className="lbl">
          {kind} · {proposal.section === "works" ? "User docs" : "Technical docs"}
        </span>
        <h2 className="text-[17px] font-semibold">{proposal.title}</h2>
        {proposal.request && <p className="text-ink2">You asked: {proposal.request}</p>}
        {proposal.agent && (
          <p className="text-ink2">
            Proposed by {who(proposal.user, proposal.agent)}
            {proposal.agent === "weacon" && ` for ${proposal.user ?? "someone who left"}`}
            {proposal.chat_id && (
              <>
                {" · "}
                <Link className="underline" to={`/chats/${proposal.chat_id}`}>
                  Open its chat
                </Link>
              </>
            )}
          </p>
        )}
        <p>{proposal.reason}</p>
      </div>
      {page && page.version !== proposal.base && (
        <p className="text-acc">The page was edited after this proposal was written. Taking a change may undo a later edit.</p>
      )}
      {changes.map((change, place) => (
        // A diff is worked out anew each time, so a change has no name but its place.
        <div key={place} className="flex flex-col gap-2">
          {change.same?.map((block, at) => (
            <pre key={at} className="font-sans whitespace-pre-wrap text-ink2">
              {block}
            </pre>
          ))}
          {!change.same && (
            <div className="flex flex-col overflow-hidden rounded-lg border border-line">
              {change.removed.map((block, at) => (
                <pre key={at} className="bg-acc/15 px-3 py-2 font-sans whitespace-pre-wrap">
                  {block}
                </pre>
              ))}
              {change.added.map((block, at) => (
                <pre key={at} className="bg-ok/15 px-3 py-2 font-sans whitespace-pre-wrap">
                  {block}
                </pre>
              ))}
              {proposal.page && proposal.markdown !== null && (
                <label className="flex items-center gap-2 border-t border-line bg-sur px-3 py-1.5">
                  <input
                    type="checkbox"
                    checked={!left.includes(place)}
                    onChange={() => setLeft(left.includes(place) ? left.filter((other) => other !== place) : [...left, place])}
                  />
                  Take this change
                </label>
              )}
            </div>
          )}
          <DocNotes notes={about(change)} />
        </div>
      ))}
      <DocNotes notes={proposal.notes.filter((note) => !placed.includes(note))} />
      <div className="flex gap-2">
        <button className="btn btn-primary" disabled={accept.isPending || left.length === changes.filter((change) => !change.same).length} onClick={() => accept.mutate(left.length ? chosen : undefined)}>
          {left.length ? "Accept the ticked changes" : proposal.markdown === null ? "Remove the page" : "Accept all"}
        </button>
        <button className="btn" disabled={reject.isPending} onClick={() => reject.mutate()}>
          Reject
        </button>
        <button className="btn" onClick={() => props.onDone()}>
          Later
        </button>
      </div>
      {error && <p className="text-acc">{error.message}</p>}
    </div>
  )
}
