import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Tabs, TabsList, TabsPanel, TabsTab } from "@ui-sdk/react/tabs"
import { useState } from "react"
import { Link, useParams, useSearchParams } from "react-router"
import { api, type CodeMap, type Docs, type Project, type Share, type Team } from "../api"
import { DocPages } from "../components/DocPages"

/** A project's generated docs, at `/projects/{id}/docs`. */
export function DocsPage({ team }: { team: Team }) {
  const { id } = useParams()
  const queryClient = useQueryClient()
  const [chosen, setChosen] = useState("")
  const [params, setParams] = useSearchParams()
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
  const tabs = [
    { value: "user", name: "User docs", pages: pages.filter((page) => page.section === "works") },
    { value: "technical", name: "Technical docs", pages: pages.filter((page) => page.section !== "works") },
  ].filter((tab) => tab.pages.length)
  const tab = tabs.find((tab) => tab.value === params.get("tab")) ?? tabs[0]
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
      {tab && (
        <Tabs value={tab.value} onValueChange={(value) => value && setParams({ tab: value })}>
          <TabsList aria-label="Docs">
            {tabs.map((tab) => (
              <TabsTab key={tab.value} value={tab.value}>
                {tab.name}
              </TabsTab>
            ))}
          </TabsList>
          {tabs.map((tab) => (
            <TabsPanel key={tab.value} value={tab.value}>
              {tab.value === "user" && <Shares path={path} />}
              <DocPages
                pages={tab.pages}
                open={params.get("page")}
                onOpen={(page) => setParams({ tab: tab.value, page })}
                code={code}
                note={
                  tab.value === "technical" &&
                  left_out.length > 0 && <p className="px-2 pt-4 text-ink2">Left out as not the project's own code: {left_out.join(", ")}</p>
                }
              />
            </TabsPanel>
          ))}
        </Tabs>
      )}
    </div>
  )
}

/** The project's public links to its user docs. */
function Shares({ path }: { path: string }) {
  const queryClient = useQueryClient()
  const shares = useQuery({ queryKey: ["shares", path], queryFn: () => api<Share[]>("GET", `${path}/shares`) })
  const changed = { onSuccess: () => queryClient.invalidateQueries({ queryKey: ["shares", path] }) }
  const share = useMutation({ mutationFn: () => api("POST", `${path}/shares`), ...changed })
  const end = useMutation({ mutationFn: (token: string) => api("DELETE", `${path}/shares/${token}`), ...changed })
  const error = share.error ?? end.error
  return (
    <div className="flex flex-col items-start gap-2 pt-3">
      <div className="flex items-center gap-2">
        <button className="btn" disabled={share.isPending} onClick={() => share.mutate()}>
          Share
        </button>
        <span className="text-ink2">Makes a public link to these pages as they are now. Anyone with the link can read them.</span>
      </div>
      {shares.data?.map(({ token, created_by, created_at }) => {
        const link = `${location.origin}/shared/${token}`
        return (
          <div key={token} className="flex flex-wrap items-center gap-2">
            <a href={link} target="_blank" rel="noreferrer" className="underline">
              Shared by {created_by} on {new Date(created_at * 1000).toLocaleString()}
            </a>
            <button className="btn" onClick={() => navigator.clipboard.writeText(link)}>
              Copy link
            </button>
            <button className="btn" disabled={end.isPending} onClick={() => end.mutate(token)}>
              Stop sharing
            </button>
          </div>
        )
      })}
      {error && <p className="text-acc">{error.message}</p>}
    </div>
  )
}
