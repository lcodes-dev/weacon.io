import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import Markdown from "markdown-to-jsx"
import { Fragment, useState } from "react"
import { Link, useParams } from "react-router"
import { api, type CodeMap, type Docs, type Project, type Team } from "./api"

const sections = { overview: "", works: "How it works", built: "How it's built" }

function safeUrl(value: string) {
  try {
    const { protocol } = new URL(value, "https://weacon.invalid")
    return /^(https?|ircs?|mailto|xmpp):$/.test(protocol) ? value : null
  } catch {
    return null
  }
}

/** A project's generated docs, at `/projects/{id}/docs`. */
export function DocsPage({ team }: { team: Team }) {
  const { id } = useParams()
  const queryClient = useQueryClient()
  const [chosen, setChosen] = useState("")
  const [open, setOpen] = useState("")
  const projects = useQuery({
    queryKey: ["projects", team.id],
    queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`),
  })
  const project = projects.data?.find((project) => project.id === id)
  const path = `/teams/${team.id}/projects/${id}`
  const docs = useQuery({
    queryKey: ["docs", id],
    queryFn: () => api<Docs>("GET", `${path}/docs`),
    // While a run is on, watch its progress.
    refetchInterval: (query) => (query.state.data?.state === "running" ? 2000 : false),
  })
  // The map says which files exist, so that only true citations become links.
  const map = useQuery({
    queryKey: ["map", id, docs.data?.branch, docs.data?.commit],
    queryFn: () => api<CodeMap>("GET", `${path}/map?branch=${encodeURIComponent(docs.data!.branch!)}`),
    enabled: !!docs.data?.branch,
  })
  const mapped = project?.branches.filter((branch) => branch.commit_sha) ?? []
  const branch = mapped.find((branch) => branch.name === chosen) ?? mapped[0]
  const start = useMutation({
    mutationFn: () => api("POST", `${path}/docs`, { branch: branch.name }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["docs", id] }),
  })
  if (!project || !docs.data) return null

  const { state, message, pages, commit, left_out } = docs.data
  const page = pages.find((page) => page.id === open) ?? pages[0]
  const stale = commit && docs.data.branch === branch?.name && commit !== branch.commit_sha
  const lines = new Map(map.data?.files.map((file) => [file.path, file.lines]))
  // `path:line` or `path:line-line` in backticks links to GitHub, when the map has those lines.
  const code = ({ children }: { children?: React.ReactNode }) => {
    const cited = /^(.+):(\d+)(?:-(\d+))?$/.exec(String(children))
    const [first, last] = cited ? [Number(cited[2]), Number(cited[3] ?? cited[2])] : [0, 0]
    if (cited && 1 <= first && first <= last && last <= (lines.get(cited[1]) ?? 0)) {
      const to = cited[3] ? `-L${cited[3]}` : ""
      const href = `https://github.com/${project.repo}/blob/${commit}/${cited[1]}#L${cited[2]}${to}`
      return (
        <a href={href} target="_blank" rel="noreferrer" title={cited[1]}>
          {/* The file's name is enough to read; the whole path is in the tooltip and the link. */}
          <code>
            {cited[1].split("/").pop()}:{cited[2]}
            {cited[3] && `-${cited[3]}`}
          </code>
        </a>
      )
    }
    return <code>{children}</code>
  }
  // An image would be fetched from its host on sight, telling it who reads what; a link waits for a click.
  const img = ({ src, alt }: { src?: string; alt?: string }) => <a href={src}>{alt || src}</a>

  return (
    <div className="page page-wide">
      <div className="flex flex-wrap items-center gap-2">
        <h1 className="mr-auto">
          <Link to={`/projects/${id}`} className="underline">
            {project.name}
          </Link>{" "}
          docs
        </h1>
        {commit && (
          <span className="text-ink2">
            Written from {docs.data.branch} at {commit.slice(0, 7)}
            {stale && ", which the branch has moved past"}
          </span>
        )}
        {mapped.length > 1 && (
          <select
            aria-label="Branch"
            className="rounded-md border border-fld bg-bg px-2 py-1"
            value={branch.name}
            onChange={(event) => setChosen(event.target.value)}
          >
            {mapped.map((branch) => (
              <option key={branch.name}>{branch.name}</option>
            ))}
          </select>
        )}
        <button className="btn btn-primary" disabled={!branch || state === "running" || start.isPending} onClick={() => start.mutate()}>
          {pages.length ? "Generate again" : "Generate docs"}
        </button>
      </div>
      {!branch && <p className="text-ink2">Docs are written from a mapped branch. Track one on the project's page first.</p>}
      {state === "running" && <p className="text-ink2">{message ?? "Starting"}… This takes a few minutes.</p>}
      {state === "failed" && <p className="text-acc">The last run failed: {message}</p>}
      {start.error && <p className="text-acc">{start.error.message}</p>}
      {page && (
        <div className="flex items-start gap-8 pt-2">
          <nav className="flex w-[200px] shrink-0 flex-col">
            {pages.map((entry, index) => (
              <div key={entry.id} className="contents">
                {entry.section !== "overview" && entry.section !== pages[index - 1]?.section && (
                  <h2 className="px-2 pb-1 text-ink2">{sections[entry.section]}</h2>
                )}
                <button
                  className={`rounded-md px-2 py-1 text-left hover:bg-hov ${entry === page ? "bg-hov font-medium" : ""}`}
                  onClick={() => setOpen(entry.id)}
                >
                  {entry.title}
                </button>
              </div>
            ))}
            {left_out.length > 0 && <p className="px-2 pt-4 text-ink2">Left out as not the project's own code: {left_out.join(", ")}</p>}
          </nav>
          <article className="prose min-w-0 flex-1">
            <h1>{page.title}</h1>
            <Markdown
              options={{
                disableParsingRawHTML: true,
                forceBlock: true,
                wrapper: Fragment,
                sanitizer: safeUrl,
                overrides: { code, img },
              }}
            >
              {page.markdown}
            </Markdown>
          </article>
        </div>
      )}
    </div>
  )
}
