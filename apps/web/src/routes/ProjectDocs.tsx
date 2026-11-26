import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { confirm } from "@ui-sdk/react/alert-dialog"
import { Menu, MenuContent, MenuItem, MenuTrigger } from "@ui-sdk/react/menu"
import { Popover, PopoverContent, PopoverDescription, PopoverTitle, PopoverTrigger } from "@ui-sdk/react/popover"
import { Tabs, TabsList, TabsPanel, TabsTab } from "@ui-sdk/react/tabs"
import { EllipsisIcon, LoaderCircleIcon, PlusIcon } from "lucide-react"
import { lazy, Suspense, useEffect, useRef, useState } from "react"
import { Link, useParams, useSearchParams } from "react-router"
import { api, type CodeMap, type Docs, type Proposal, type Share, type Team } from "../api"
import { DocHistory } from "../components/DocHistory"
import { PageNotes } from "../components/DocNotes"
import { DocArticle, DocPages } from "../components/DocPages"
import { DocProposal } from "../components/DocProposal"
import { useProjects } from "../queries"

const DocEditor = lazy(() => import("../components/DocEditor").then((module) => ({ default: module.DocEditor })))

export function DocsPage({ team }: { team: Team }) {
  const { id } = useParams()
  const queryClient = useQueryClient()
  const [chosen, setChosen] = useState("")
  const [params, setParams] = useSearchParams()
  const [view, setView] = useState<{ page: string; mode: "edit" | "history" }>()
  const [proposed, setProposed] = useState("")
  const [rejected, setRejected] = useState<Proposal>()
  const [asked, setAsked] = useState("")
  const projects = useProjects(team)
  const project = projects.data?.find((project) => project.id === id)
  const path = `/teams/${team.id}/projects/${id}`
  const docs = useQuery({
    queryKey: ["docs", id],
    queryFn: () => api<Docs>("GET", `${path}/docs`),
  })
  const map = useQuery({
    queryKey: ["map", id, docs.data?.branch, docs.data?.commit],
    queryFn: () => api<CodeMap>("GET", `${path}/map?branch=${encodeURIComponent(docs.data!.branch!)}`),
    enabled: !!docs.data?.branch,
  })
  const mapped = project?.branches.filter((branch) => branch.commit_sha) ?? []
  const branch = mapped.find((branch) => branch.name === chosen) ?? mapped[0]
  const changed = { onSuccess: () => queryClient.invalidateQueries({ queryKey: ["docs", id] }) }
  const start = useMutation({ mutationFn: () => api("POST", `${path}/docs`, { branch: branch.name }), ...changed })
  const add = useMutation({
    mutationFn: (section: string) => api<{ id: string }>("POST", `${path}/docs/pages`, { section, title: "New page" }),
    onSuccess: async ({ id: page }, section) => {
      await changed.onSuccess()
      setParams({ tab: section === "works" ? "user" : "technical", page })
      setView({ page, mode: "edit" })
    },
  })
  const order = useMutation({
    mutationFn: (ids: string[]) => api("PUT", `${path}/docs/pages`, ids),
    onMutate: (ids) =>
      queryClient.setQueryData<Docs>(["docs", id], (docs) => docs && { ...docs, pages: ids.flatMap((id) => docs.pages.find((page) => page.id === id) ?? []) }),
    onSettled: changed.onSuccess,
  })
  const remove = useMutation({ mutationFn: (page: string) => api("DELETE", `${path}/docs/pages/${page}`), ...changed })
  const ask = useMutation({
    mutationFn: (page?: string) => {
      const earlier = rejected?.request && `${rejected.request}\n\nWhat was proposed for it:\n${rejected.markdown ?? "Removing the page"}`
      return api("POST", `${path}/docs/requests`, { text: asked, page, branch: branch.name, earlier })
    },
    onSuccess: () => {
      setAsked("")
      setRejected(undefined)
      return changed.onSuccess()
    },
  })
  const [unchanged, setUnchanged] = useState(false)
  const before = useRef(docs.data)
  useEffect(() => {
    const was = before.current
    before.current = docs.data
    if (docs.data?.state === "running") setUnchanged(false)
    else if (was?.state === "running" && docs.data?.state === "idle") setUnchanged(was.pages.length > 0 && !docs.data.proposals.length)
  }, [docs.data])
  if (!project || !docs.data) return null

  const { state, message, pages, commit, left_out, proposals } = docs.data
  const proposal = proposals.find((proposal) => proposal.id === proposed)
  const tabs = [
    { value: "user", name: "User docs", section: "works", pages: pages.filter((page) => page.section === "works") },
    { value: "technical", name: "Technical docs", section: "built", pages: pages.filter((page) => page.section !== "works") },
  ]
  const tab = tabs.find((tab) => tab.value === params.get("tab")) ?? tabs.find((tab) => tab.pages.length) ?? tabs[0]
  const open = tab.pages.find((page) => page.id === params.get("page")) ?? tab.pages[0]
  const file = (file: string) => `/api${path}/files/${file}`
  const error = start.error ?? add.error ?? order.error ?? remove.error ?? ask.error
  const running = state === "running" || start.isPending || ask.isPending
  const stale = commit && docs.data.branch === branch?.name && commit !== branch.commit_sha
  const lines = new Map(map.data?.files.map((file) => [file.path, file.lines]))
  const citing = (from: string | null) => ({ children }: { children?: React.ReactNode }) => {
    const cited = /^(.+):(\d+)(?:-(\d+))?$/.exec(String(children))
    const [first, last] = cited ? [Number(cited[2]), Number(cited[3] ?? cited[2])] : [0, 0]
    if (cited && from && 1 <= first && first <= last && last <= (lines.get(cited[1]) ?? 0)) {
      const to = cited[3] ? `-L${cited[3]}` : ""
      const href = `https://github.com/${project.repo}/blob/${from}/${cited[1]}#L${cited[2]}${to}`
      return (
        <a href={href} target="_blank" rel="noreferrer" title={cited[1]}>
          <code>
            {cited[1].split("/").pop()}:{cited[2]}
            {cited[3] && `-${cited[3]}`}
          </code>
        </a>
      )
    }
    return <code>{children}</code>
  }
  const leave = async () => {
    if (view?.mode !== "edit") return true
    const sure = await confirm({
      title: "Leave the editor?",
      description: "What you typed and haven't saved is lost.",
      confirmLabel: "Leave",
    })
    if (sure) setView(undefined)
    return sure
  }
  return (
    <div className="page page-wide">
      <div className="flex flex-wrap items-center gap-2">
        <div className="mr-auto flex flex-col gap-0.5">
          <h1>
            <Link to={`/projects/${id}`} className="underline">
              {project.name}
            </Link>{" "}
            docs
          </h1>
          {commit && (
            <span className="lbl">
              {docs.data.branch} · {commit.slice(0, 7)}
              {stale && " · the branch has moved on"}
            </span>
          )}
        </div>
        {mapped.length > 1 && (
          <select
            aria-label="Branch"
            className="h-[30px] rounded-lg border border-fld bg-bg px-2"
            value={branch.name}
            onChange={(event) => setChosen(event.target.value)}
          >
            {mapped.map((branch) => (
              <option key={branch.name}>{branch.name}</option>
            ))}
          </select>
        )}
        <button className="btn btn-primary" disabled={!branch || running} onClick={() => start.mutate()}>
          {pages.length ? "Update docs" : "Generate docs"}
        </button>
      </div>
      {!branch && <p className="text-ink2">Docs are written from a mapped branch. Track one on the project's page first.</p>}
      {running && (
        <div role="status" className="flex items-center gap-3 rounded-lg border border-line bg-sur px-3 py-2">
          <LoaderCircleIcon aria-hidden="true" className="size-4 shrink-0 animate-spin text-ink2" />
          <div className="flex flex-col">
            <span className="font-medium">{(state === "running" && message) || "Starting"}…</span>
            <span className="text-ink2">
              The docs writer is working in the background, which takes a few minutes. You can leave this page; {pages.length ? "nothing changes until you accept what it proposes" : "the pages appear here when it is done"}.
            </span>
          </div>
        </div>
      )}
      {state === "failed" && !running && (
        <div role="alert" className="flex flex-col gap-0.5 rounded-lg border border-acc/40 bg-acc/10 px-3 py-2">
          <span className="font-medium">The last run failed, and changed nothing</span>
          <span>{message}</span>
          <span className="text-ink2">
            The docs writer is chosen in{" "}
            <Link to="/settings/team" className="underline">
              Team settings
            </Link>
            . Fix what it says there or on the server, then try again.
          </span>
        </div>
      )}
      {unchanged && <p role="status" className="text-ink2">The writer went over the docs and found nothing to change.</p>}
      {error && <p className="text-acc">{error.message}</p>}
      {proposals.length > 0 && !proposal && (
        <div className="flex flex-wrap items-center gap-2 rounded-lg border border-amb-line bg-amb-soft px-3 py-2">
          <span className="mr-1 font-medium">
            {proposals.length} {proposals.length === 1 ? "proposal" : "proposals"} to review
          </span>
          {proposals.map((proposal) => (
            <button key={proposal.id} className="btn btn-sm bg-bg" onClick={async () => (await leave()) && setProposed(proposal.id)}>
              {!proposal.page ? "New: " : proposal.markdown === null ? "Remove: " : ""}
              {proposal.title}
            </button>
          ))}
        </div>
      )}
      {proposal && (
        <DocProposal
          key={proposal.id}
          path={path}
          proposal={proposal}
          page={pages.find((page) => page.id === proposal.page)}
          onDone={(rejected) => {
            setProposed("")
            if (rejected) setRejected(rejected)
          }}
        />
      )}
      {!proposal && (
        <>
          <form
            className="flex gap-2"
            onSubmit={(event) => {
              event.preventDefault()
              ask.mutate(open?.id)
            }}
          >
            <input
              aria-label="Ask for a change"
              className="h-[30px] min-w-0 flex-1 rounded-lg border border-fld bg-bg px-2.5 outline-none focus:border-ink2"
              placeholder={open ? `Ask the writer for a change, such as "explain the last step of ${open.title} better" or "add a page on…"` : "Ask the writer for a page"}
              value={asked}
              onChange={(event) => setAsked(event.target.value)}
            />
            <button className="btn" disabled={!branch || !asked.trim() || running}>
              Ask
            </button>
          </form>
          <Tabs value={tab.value} onValueChange={async (value) => value && (await leave()) && setParams({ tab: value })}>
            <div className="flex items-center border-b border-line">
              <TabsList aria-label="Docs" className="mr-auto">
                {tabs.map((tab) => (
                  <TabsTab key={tab.value} value={tab.value}>
                    {tab.name}
                  </TabsTab>
                ))}
              </TabsList>
              {tab.value === "user" && <Shares path={path} empty={!tab.pages.length} />}
            </div>
            {tabs.map((tab) => (
              <TabsPanel key={tab.value} value={tab.value}>
                <DocPages
                  pages={tab.pages}
                  open={params.get("page")}
                  onOpen={async (page) => (await leave()) && setParams({ tab: tab.value, page })}
                  onOrder={(ids) => order.mutate([...pages.filter((page) => !tab.pages.includes(page)).map((page) => page.id), ...ids])}
                  file={file}
                  note={
                    <>
                      <button
                        className="flex items-center gap-1.5 rounded-md py-1 pr-2 pl-5 text-left text-ink2 phone:min-h-11 hover:bg-hov hover:text-ink"
                        disabled={add.isPending}
                        onClick={async () => (await leave()) && add.mutate(tab.section)}
                      >
                        <PlusIcon aria-hidden="true" className="size-3.5" />
                        Add page
                      </button>
                      {tab.value === "technical" && left_out.length > 0 && (
                        <p className="pt-4 pl-5 text-ink2">Left out as not the project's own code: {left_out.join(", ")}</p>
                      )}
                    </>
                  }
                >
                  {(page) => {
                    const mode = view?.page === page.id ? view.mode : undefined
                    const done = () => setView(undefined)
                    if (mode === "edit")
                      return (
                        <Suspense>
                          <DocEditor key={page.id} path={path} page={page} onDone={done} />
                        </Suspense>
                      )
                    if (mode === "history") return <DocHistory key={page.id} path={path} page={page} onDone={done} />
                    const act = async (action: string) => {
                      if (action === "history") return setView({ page: page.id, mode: "history" })
                      const sure = await confirm({
                        title: `Delete "${page.title}"?`,
                        description: "The page and its history are deleted for everyone on the team. A public link made earlier keeps its copy.",
                        confirmLabel: "Delete",
                      })
                      if (sure) remove.mutate(page.id)
                    }
                    return (
                      <div className="flex min-w-0 flex-1 items-start gap-3">
                        <div className="flex min-w-0 flex-1 flex-col gap-3">
                          <DocArticle {...page} code={citing(page.commit)} file={file} />
                          {page.section === "works" && page.files.length > 0 && <p className="text-ink2">Written from: {page.files.join(", ")}</p>}
                          {page.unread.length > 0 && <p className="text-ink2">Not read, for lack of room: {page.unread.join(", ")}</p>}
                          <PageNotes path={path} page={page} />
                    </div>
                        <button className="btn" onClick={() => setView({ page: page.id, mode: "edit" })}>
                          Edit
                        </button>
                        <Menu onAction={(action) => void act(action)}>
                          <MenuTrigger aria-label="More" className="icb">
                            <EllipsisIcon aria-hidden="true" />
                          </MenuTrigger>
                          <MenuContent>
                            <MenuItem value="history">History</MenuItem>
                            <MenuItem value="delete">Delete</MenuItem>
                          </MenuContent>
                        </Menu>
                      </div>
                    )
                  }}
                </DocPages>
              </TabsPanel>
            ))}
          </Tabs>
        </>
      )}
    </div>
  )
}

function Shares({ path, empty }: { path: string; empty: boolean }) {
  const queryClient = useQueryClient()
  const [copied, setCopied] = useState("")
  const shares = useQuery({ queryKey: ["shares", path], queryFn: () => api<Share[]>("GET", `${path}/shares`) })
  const changed = { onSuccess: () => queryClient.invalidateQueries({ queryKey: ["shares", path] }) }
  const share = useMutation({ mutationFn: () => api("POST", `${path}/shares`), ...changed })
  const end = useMutation({ mutationFn: (token: string) => api("DELETE", `${path}/shares/${token}`), ...changed })
  const error = share.error ?? end.error
  return (
    <Popover>
      <PopoverTrigger className="btn btn-sm mb-1">Share{shares.data?.length ? ` · ${shares.data.length}` : ""}</PopoverTrigger>
      <PopoverContent>
        <PopoverTitle>Share the user docs</PopoverTitle>
        <PopoverDescription>
          A link shows these pages as they are now, to anyone who has it and without an account. Later edits need a new link.
        </PopoverDescription>
        <button className="btn btn-primary self-start" disabled={empty || share.isPending} onClick={() => share.mutate()}>
          Create link
        </button>
        {shares.data?.map(({ token, created_by, created_at }) => {
          const link = `${location.origin}/shared/${token}`
          return (
            <div key={token} className="flex items-center gap-2 border-t border-line pt-2">
              <a href={link} target="_blank" rel="noreferrer" className="mr-auto flex min-w-0 flex-col">
                <span className="underline">{new Date(created_at * 1000).toLocaleString()}</span>
                <span className="text-ink2">by {created_by}</span>
              </a>
              <button className="btn btn-sm" onClick={() => navigator.clipboard.writeText(link).then(() => setCopied(token))}>
                {copied === token ? "Copied" : "Copy"}
              </button>
              <button className="btn btn-sm" disabled={end.isPending} onClick={() => end.mutate(token)}>
                Stop sharing
              </button>
            </div>
          )
        })}
        {error && <p className="text-acc">{error.message}</p>}
      </PopoverContent>
    </Popover>
  )
}
