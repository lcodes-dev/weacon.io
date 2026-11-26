import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useState } from "react"
import { api, type DocsPage, type DocVersion } from "../api"
import { Counts, Diff } from "./Diff"

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
