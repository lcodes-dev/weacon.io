import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { diffArrays } from "diff"
import { type ReactNode, useState } from "react"
import { Link } from "react-router"
import { api, type DocsPage, type DocVersion, type Note, type Proposal, who } from "../api"
import { Counts, Diff } from "./Diff"

const blocks = (text: string) => (text.trim() ? text.trim().split(/\n{2,}/) : [])

export function DocProposal(props: { path: string; proposal: Proposal; page?: DocsPage; onDone: (rejected?: Proposal) => void }) {
  const { path, proposal, page } = props
  const queryClient = useQueryClient()
  const [left, setLeft] = useState<number[]>([])
  const done = (rejected?: Proposal) => queryClient.invalidateQueries({ queryKey: ["docs"] }).then(() => props.onDone(rejected))
  const accept = useMutation({
    mutationFn: (markdown?: string) => api("POST", `${path}/docs/proposals/${proposal.id}`, { markdown, version: page?.version }),
    onSuccess: () => done(),
    onError: () => queryClient.invalidateQueries({ queryKey: ["docs"] }),
  })
  const reject = useMutation({
    mutationFn: () => api("DELETE", `${path}/docs/proposals/${proposal.id}`),
    onSuccess: () => done(proposal),
  })
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

function DocNotes({ notes, actions }: { notes: Note[]; actions?: (note: Note, place: number) => ReactNode }) {
  return notes.map((note, place) => (
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

export function PageNotes({ path, page }: { path: string; page: DocsPage }) {
  const queryClient = useQueryClient()
  const changed = { onSuccess: () => queryClient.invalidateQueries({ queryKey: ["docs"] }) }
  const dismiss = useMutation({ mutationFn: (place: number) => api("DELETE", `${path}/docs/pages/${page.id}/notes/${place}`), ...changed })
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

export function DocHistory({ path, page, onDone }: { path: string; page: DocsPage; onDone: () => void }) {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState<{ id: string; against: "before" | "now" }>()
  const versions = useQuery({
    queryKey: ["docs", "versions", page.id, page.version],
    queryFn: () => api<DocVersion[]>("GET", `${path}/docs/pages/${page.id}/versions`),
  })
  const restore = useMutation({
    mutationFn: ({ title, markdown }: DocVersion) => api("PUT", `${path}/docs/pages/${page.id}`, { title, markdown, version: page.version }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["docs"] }).then(onDone),
  })
  if (!versions.data) return null
  const list = versions.data
  return (
    <div className="flex max-w-[720px] min-w-0 flex-1 flex-col gap-3">
      <div className="flex items-center justify-between gap-2">
        <h1 className="text-[17px] font-semibold">History of {page.title}</h1>
        <button className="btn" onClick={onDone}>
          Back to the page
        </button>
      </div>
      {restore.error && <p className="text-acc">{restore.error.message}</p>}
      <ol className="flex flex-col">
        {list.map((version, place) => {
          const before = list[place + 1]?.markdown ?? ""
          const when = new Date(version.created_at * 1000).toLocaleString()
          const shown = open?.id === version.id ? open.against : undefined
          const show = (against: "before" | "now") => setOpen(shown === against ? undefined : { id: version.id, against })
          return (
            <li key={version.id} className="flex flex-col gap-2 border-b border-line py-2">
              <div className="flex flex-wrap items-center gap-2">
                <button className="flex min-w-0 flex-1 items-center gap-2 text-left" aria-expanded={shown === "before"} onClick={() => show("before")}>
                  <span className="font-medium">{version.author ?? "The docs writer"}</span>
                  <span className="text-ink2">{when}</span>
                  <Counts from={before} to={version.markdown} />
                  {place === 0 && <span className="lbl">Current</span>}
                </button>
                {place > 0 && (
                  <>
                    <button className="btn btn-sm" aria-expanded={shown === "now"} onClick={() => show("now")}>
                      Compare with now
                    </button>
                    <button className="btn btn-sm" disabled={restore.isPending} onClick={() => restore.mutate(version)}>
                      Restore
                    </button>
                  </>
                )}
              </div>
              {shown === "before" && <Diff title={`What ${when} changed`} from={before} to={version.markdown} />}
              {shown === "now" && <Diff title={`From ${when} to now`} from={version.markdown} to={list[0].markdown} />}
            </li>
          )
        })}
      </ol>
    </div>
  )
}
