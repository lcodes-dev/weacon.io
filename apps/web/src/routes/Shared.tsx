import { useQuery } from "@tanstack/react-query"
import { useState } from "react"
import { useParams } from "react-router"
import { api, type Shared as SharedDocs } from "../api"
import { DocPages } from "../components/DocPages"

/** A public link to a copy of a project's user docs, at `/shared/{token}`. Needs no account. */
export function Shared() {
  const { token } = useParams()
  const [open, setOpen] = useState<string | null>(null)
  const shared = useQuery({
    queryKey: ["shared", token],
    queryFn: () => api<SharedDocs>("GET", `/shares/${token}`),
    retry: false,
  })
  if (shared.error) return <p className="flex h-screen items-center justify-center text-ink2">This link no longer works</p>
  if (!shared.data) return null
  const { title, created_at, pages } = shared.data
  return (
    <div className="page page-wide">
      <meta name="robots" content="noindex" />
      <title>{`${title} docs`}</title>
      <div className="flex flex-wrap items-baseline gap-2">
        <h1 className="mr-auto">{title} docs</h1>
        <span className="text-ink2">Shared on {new Date(created_at * 1000).toLocaleDateString()}</span>
      </div>
      {/* A copy's pages never change, so a page's place names it. */}
      <DocPages pages={pages.map((page, place) => ({ ...page, id: String(place) }))} open={open} onOpen={setOpen} file={(file) => `/api/shares/${token}/files/${file}`} />
    </div>
  )
}
