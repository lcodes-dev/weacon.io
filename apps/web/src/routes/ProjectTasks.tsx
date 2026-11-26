import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { XIcon } from "lucide-react"
import { useState } from "react"
import { Link, useNavigate, useParams, useSearchParams } from "react-router"
import { agents, api, statuses, type Chat, type Docs, type Project, type Status, type Task, type Team, type TeamDetails, type User } from "../api"
import { Board, BoardCard, BoardColumn } from "../components/Board"

const statusKeys = Object.keys(statuses) as Status[]

/** A project's tasks, which every page that shows one asks for the same way. */
export function useTasks(team: Team, project?: string) {
  return useQuery({
    queryKey: ["tasks", project],
    queryFn: () => api<Task[]>("GET", `/teams/${team.id}/projects/${project}/tasks`),
    enabled: !!project,
  })
}

/** The page at `/projects/{id}/tasks`: the project's tasks as a list or a board, and the one picked beside it. */
export function TasksPage({ team, user }: { team: Team; user: User }) {
  const { id } = useParams()
  const [params] = useSearchParams()
  const navigate = useNavigate()
  const queryClient = useQueryClient()
  const projects = useQuery({ queryKey: ["projects", team.id], queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`) })
  const tasks = useTasks(team, id)
  const path = `/teams/${team.id}/projects/${id}/tasks`
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["tasks", id] })
  const order = useMutation({
    mutationFn: (order: { status: string; tasks: string[] }) => api("PUT", path, order),
    onSettled: refresh,
  })
  // A task is made at once and then filled in, as a page of the docs is.
  const create = useMutation({
    mutationFn: () => api<{ id: string }>("POST", path, { title: "New task" }),
    onSuccess: async ({ id }) => {
      await refresh()
      navigate(to(id))
    },
  })
  const project = projects.data?.find((project) => project.id === id)
  if (!projects.data || !tasks.data) return null
  if (!project) return <p className="page text-ink2">This project isn't in {team.name}.</p>

  const board = params.get("view") === "board"
  const picked = params.get("task")
  const to = (task?: string, view = board) => `?${new URLSearchParams({ ...(view && { view: "board" }), ...(task && { task }) })}`
  const name = (task: Task) => `${project.key}-${task.number}`
  const byId = new Map(tasks.data.map((task) => [task.id, task]))
  const lists = Object.fromEntries(statusKeys.map((status) => [status, tasks.data.filter((task) => task.status === status).map((task) => task.id)]))
  // The card stays where it was dropped while the server is told.
  const place = (lists: Record<string, string[]>, { to }: { to: string }) => {
    const moved = statusKeys.flatMap((status) => lists[status].flatMap((id) => (byId.has(id) ? [{ ...byId.get(id)!, status }] : [])))
    queryClient.setQueryData(["tasks", id], moved)
    order.mutate({ status: to, tasks: lists[to] })
  }
  const task = picked ? byId.get(picked) : undefined

  return (
    <div className="flex h-full flex-col">
      <header className="flex items-center gap-3 border-b border-line px-6 py-3">
        <Link to={`/projects/${project.id}`} className="text-[16px] font-semibold">
          {project.name}
        </Link>
        <span className="lbl">Tasks</span>
        <nav aria-label="View" className="flex gap-1">
          {[false, true].map((view) => (
            <Link key={String(view)} to={to(picked ?? undefined, view)} aria-current={view === board ? "page" : undefined} className="btn btn-sm aria-[current]:bg-hov">
              {view ? "Board" : "List"}
            </Link>
          ))}
        </nav>
        <span className="flex-1" />
        <button className="btn btn-primary" disabled={create.isPending} onClick={() => create.mutate()}>
          New task
        </button>
      </header>
      <div className="flex min-h-0 flex-1">
        <div className="min-w-0 flex-1 overflow-auto p-6">
          {(order.error ?? create.error) && <p className="pb-2 text-acc">{(order.error ?? create.error)?.message}</p>}
          {tasks.data.length === 0 && <p className="text-ink2">{project.name} has no tasks yet.</p>}
          {board ? (
            <Board value={lists} onValueChange={place} className="grid min-w-[600px] grid-cols-3 items-start gap-3">
              {({ lists }) =>
                statusKeys.map((status) => (
                  <section key={status} className="flex flex-col gap-2 rounded-xl border border-line bg-side p-2">
                    <h2 className="lbl px-1 pt-1">
                      {statuses[status]} · {lists[status]?.length ?? 0}
                    </h2>
                    <BoardColumn value={status} aria-label={statuses[status]} className="flex min-h-12 flex-col gap-2">
                      {lists[status]?.flatMap((id) => {
                        const task = byId.get(id)
                        if (!task) return []
                        return (
                          <BoardCard key={id} value={id} tabIndex={0} aria-current={id === picked ? "true" : undefined} aria-describedby="board-keys">
                            <span className="font-mono text-[11px] text-ink3">{name(task)}</span>
                            <Link to={to(id)} draggable={false} className="font-medium">
                              {task.title}
                            </Link>
                            {task.owner && <span className="text-[12px] text-ink2">{task.owner}</span>}
                          </BoardCard>
                        )
                      })}
                    </BoardColumn>
                  </section>
                ))
              }
            </Board>
          ) : (
            statusKeys.map((status) => {
              const listed = tasks.data.filter((task) => task.status === status)
              return (
                listed.length > 0 && (
                  <section key={status} className="flex flex-col pb-4">
                    <h2 className="lbl px-2 pb-1">
                      {statuses[status]} · {listed.length}
                    </h2>
                    {listed.map((task) => (
                      <Link
                        key={task.id}
                        to={to(task.id)}
                        aria-current={task.id === picked ? "true" : undefined}
                        className="flex items-baseline gap-3 rounded-md px-2 py-1.5 hover:bg-hov aria-[current]:bg-hov"
                      >
                        <span className="w-16 shrink-0 font-mono text-[12px] text-ink3">{name(task)}</span>
                        <span className="min-w-0 flex-1 truncate">{task.title}</span>
                        <span className="text-ink2">{task.owner}</span>
                      </Link>
                    ))}
                  </section>
                )
              )
            })
          )}
          <p id="board-keys" hidden>
            Press Space to pick the card up, the arrows to move it, and Space to drop it.
          </p>
        </div>
        {task && (
          <Panel
            key={task.id}
            team={team}
            user={user}
            project={project}
            task={task}
            name={name(task)}
            path={`${path}/${task.id}`}
            close={to()}
          />
        )}
      </div>
    </div>
  )
}

type PanelProps = { team: Team; user: User; project: Project; task: Task; name: string; path: string; close: string }

/** One task: what it is, changed in place, and the chats that work on it. */
function Panel({ team, user, project, task, name, path, close }: PanelProps) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [title, setTitle] = useState(task.title)
  const [text, setText] = useState(task.text)
  const [check, setCheck] = useState("")
  const details = useQuery({ queryKey: ["team", team.id], queryFn: () => api<TeamDetails>("GET", `/teams/${team.id}`) })
  const docs = useQuery({ queryKey: ["docs", project.id], queryFn: () => api<Docs>("GET", `/teams/${team.id}/projects/${project.id}/docs`) })
  const chats = useQuery({ queryKey: ["chats", team.id], queryFn: () => api<Chat[]>("GET", `/teams/${team.id}/chats`) })
  // The pages its title and text are about, which the server finds by meaning.
  const offered = useQuery({
    queryKey: ["task-docs", task.id, task.title, task.text, task.docs],
    queryFn: () => api<string[]>("GET", `${path}/docs`),
  })
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["tasks", project.id] })
  const change = useMutation({
    mutationFn: (fields: Partial<Pick<Task, "title" | "text" | "checks" | "owner_id" | "docs" | "status">>) => api("PUT", path, fields),
    onSuccess: refresh,
  })
  const remove = useMutation({
    mutationFn: () => api("DELETE", path),
    onSuccess: async () => {
      await refresh()
      navigate(close)
    },
  })
  const error = change.error ?? remove.error
  const pages = docs.data?.pages ?? []
  const theirs = chats.data?.filter((chat) => chat.task_id === task.id) ?? []
  const from = chats.data?.find((chat) => chat.id === task.chat_id)
  const written = [...new Set(pages.filter((page) => task.docs.includes(page.id)).flatMap((page) => page.files))]
  const select = "rounded-md border border-fld bg-bg px-2 py-1"

  return (
    <aside aria-label={name} className="flex w-[360px] shrink-0 flex-col gap-3 overflow-auto border-l border-line bg-side p-5">
      <div className="flex items-center gap-2">
        <span className="font-mono text-[12px] text-ink3">{name}</span>
        <select aria-label="Status" className={select} value={task.status} onChange={(event) => change.mutate({ status: event.target.value as Status })}>
          {statusKeys.map((status) => (
            <option key={status} value={status}>
              {statuses[status]}
            </option>
          ))}
        </select>
        <span className="flex-1" />
        <Link to={close} aria-label="Close" className="icb">
          <XIcon aria-hidden="true" />
        </Link>
      </div>
      <form
        className="flex flex-col gap-3"
        onSubmit={(event) => {
          event.preventDefault()
          change.mutate({ title, text })
        }}
      >
        <Field>
          <FieldLabel>Title</FieldLabel>
          <Input value={title} onChange={(event) => setTitle(event.target.value)} required />
        </Field>
        <Field>
          <FieldLabel>What is wanted</FieldLabel>
          <FieldControl render={<textarea />} className="min-h-28 rounded-md border border-fld bg-bg px-3 py-2" value={text} onChange={(event) => setText(event.target.value)} />
        </Field>
        {(title !== task.title || text !== task.text) && <button className="btn self-start">Save</button>}
      </form>
      <p className="text-ink2">
        Asked by {task.asker ?? "someone who left"} on {new Date(task.created_at * 1000).toLocaleDateString()}
        {from && (
          <>
            , in the chat{" "}
            <Link to={`/chats/${from.id}`} className="underline">
              {from.title}
            </Link>
          </>
        )}
      </p>
      <label className="flex flex-col gap-1">
        <span className="lbl">Owner</span>
        <select className={select} value={task.owner_id ?? ""} onChange={(event) => change.mutate({ owner_id: event.target.value || null })}>
          <option value="">Nobody</option>
          {details.data?.members.map((member) => (
            <option key={member.id} value={member.id}>
              {member.name}
            </option>
          ))}
        </select>
      </label>

      <h3 className="lbl pt-2">Done when</h3>
      {task.checks.map((item, place) => (
        <div key={place} className="group flex items-start gap-2">
          <label className="flex flex-1 items-start gap-2">
            <input
              type="checkbox"
              className="mt-1"
              checked={item.done}
              onChange={(event) => change.mutate({ checks: task.checks.map((other, at) => (at === place ? { ...other, done: event.target.checked } : other)) })}
            />
            <span className={item.done ? "text-ink2 line-through" : ""}>{item.text}</span>
          </label>
          <button type="button" aria-label={`Remove ${item.text}`} className="icb size-5" onClick={() => change.mutate({ checks: task.checks.filter((_, at) => at !== place) })}>
            <XIcon aria-hidden="true" />
          </button>
        </div>
      ))}
      <form
        onSubmit={(event) => {
          event.preventDefault()
          change.mutate({ checks: [...task.checks, { text: check, done: false }] })
          setCheck("")
        }}
      >
        <Input aria-label="Add a check" placeholder="Add a check" value={check} onChange={(event) => setCheck(event.target.value)} />
      </form>

      <h3 className="lbl pt-2">Docs it touches</h3>
      {task.docs.flatMap((id) => {
        const page = pages.find((page) => page.id === id)
        if (!page) return []
        const tab = page.section === "works" ? "user" : "technical"
        return (
          <div key={id} className="flex items-center gap-2">
            <Link to={`/projects/${project.id}/docs?tab=${tab}&page=${id}`} className="flex-1 underline">
              {page.title}
            </Link>
            <button type="button" aria-label={`Remove ${page.title}`} className="icb size-5" onClick={() => change.mutate({ docs: task.docs.filter((other) => other !== id) })}>
              <XIcon aria-hidden="true" />
            </button>
          </div>
        )
      })}
      {pages.some((page) => !task.docs.includes(page.id)) && (
        <select aria-label="Add a page" className={select} value="" onChange={(event) => change.mutate({ docs: [...task.docs, event.target.value] })}>
          <option value="">Add a page…</option>
          {pages
            .filter((page) => !task.docs.includes(page.id))
            .map((page) => (
              <option key={page.id} value={page.id}>
                {page.title}
              </option>
            ))}
        </select>
      )}
      {docs.data && pages.length === 0 && <p className="text-ink2">{project.name} has no docs yet.</p>}
      {offered.data?.flatMap((id) => {
        const page = pages.find((page) => page.id === id)
        if (!page) return []
        return (
          <button key={id} type="button" className="btn btn-sm self-start" onClick={() => change.mutate({ docs: [...task.docs, id] })}>
            Add {page.title}
          </button>
        )
      })}
      {offered.error && <p className="text-ink2">{offered.error.message}</p>}
      {written.length > 0 && (
        <>
          <h3 className="lbl pt-2">Those pages were written from</h3>
          {written.map((file) => (
            <code key={file} className="shrink-0 truncate font-mono text-[12px] text-ink2">
              {file}
            </code>
          ))}
        </>
      )}

      <h3 className="lbl pt-2">Chats</h3>
      {theirs.length === 0 && <p className="text-ink2">None yet</p>}
      {theirs.map((chat) => (
        <div key={chat.id} className="flex items-center gap-2">
          <span className={`size-2 shrink-0 rounded-full ${chat.running ? "animate-pulse bg-ok" : "bg-fld"}`} />
          <Link to={`/chats/${chat.id}`} className="min-w-0 flex-1 truncate underline">
            {chat.title}
          </Link>
          <span className="shrink-0 text-[12px] text-ink2">
            {chat.user} · {agents[chat.agent].name}
          </span>
          {chat.pull_url && (
            <a href={chat.pull_url} target="_blank" rel="noreferrer" className="shrink-0 underline">
              #{chat.pull_number}
            </a>
          )}
        </div>
      ))}

      <div className="flex gap-2 pt-2">
        <Link to={`/chats/new?project=${project.id}&task=${task.id}`} className="btn btn-primary">
          New chat for this task
        </Link>
        {(task.asker_id === user.id || team.role === "owner") && (
          <button type="button" className="btn" onClick={() => remove.mutate()}>
            Delete
          </button>
        )}
      </div>
      {error && <p className="text-acc">{error.message}</p>}
    </aside>
  )
}
