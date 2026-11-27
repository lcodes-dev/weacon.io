import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect, useRef, useState } from "react"
import { Link, useNavigate, useParams } from "react-router"
import { agents, api, type OpenChat, type Team } from "../api"
import { ChatEvents, useChatChange } from "./ChatEvents"

type Part = { chat: OpenChat; path: string }

export function ChatPage({ team }: { team: Team }) {
  const path = `/teams/${team.id}/chats/${useParams().id}`
  const queryClient = useQueryClient()
  const chat = useQuery({
    queryKey: ["chat", path],
    queryFn: async () => {
      const had = queryClient.getQueryData<OpenChat>(["chat", path])?.events ?? []
      const now = await api<OpenChat>("GET", `${path}?after=${had.at(-1)?.id ?? ""}`)
      return { ...now, events: [...had, ...now.events] }
    },
  })

  const end = useRef<HTMLDivElement>(null)
  const told = chat.data?.events.length
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" })
  }, [told])

  if (chat.error) return <p className="page text-ink2">{chat.error.message}</p>
  if (!chat.data) return null
  const { machine, connected, running, mine, events } = chat.data
  const changed = events.some((event) => event.kind === "done" && event.changed.length > 0)
  return (
    <div className="flex h-full flex-col">
      <Header chat={chat.data} path={path} team={team} />
      <div className="mx-auto flex w-full max-w-3xl flex-1 flex-col gap-3 overflow-auto px-6 py-4 phone:px-4">
        <ChatEvents chat={chat.data} path={path} />
        {running && <p className="animate-pulse text-ink2">{connected ? "Working…" : `Waiting for ${machine} to connect…`}</p>}
        <div ref={end} />
      </div>
      <div className="mx-auto flex w-full max-w-3xl flex-col gap-3 px-6 pb-4 phone:px-3 phone:pb-[max(12px,env(safe-area-inset-bottom))]">
        {mine && machine && changed && !running && <Push chat={chat.data} path={path} />}
        {mine && machine && !chat.data.group_id && <Ask chat={chat.data} path={path} />}
      </div>
    </div>
  )
}

function Header({ chat, path, team }: Part & { team: Team }) {
  const { title, project, branch, agent, machine, connected, mine, running, user, hosts, pull_number, pull_url, task } = chat
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const forget = useChatChange(path, (host: string) => api("DELETE", `${path}/hosts/${host}`))
  const remove = useMutation({
    mutationFn: () => api("DELETE", path),
    onSuccess: async () => {
      await queryClient.invalidateQueries({ queryKey: ["chats", team.id] })
      navigate("/")
    },
  })
  return (
    <header className="bar">
      <div className="flex w-full items-center gap-3">
        <h1 className="flex-1 text-[15px] font-semibold">{title}</h1>
        {mine && !running && (
          <button type="button" className="btn btn-sm" disabled={remove.isPending} onClick={() => remove.mutate()}>
            Delete chat
          </button>
        )}
      </div>
      {remove.error && <p className="w-full text-acc">{remove.error.message}</p>}
      <span className="lbl flex w-full flex-wrap items-center gap-2">
        {project} · {branch} · {agents[agent].name} ·
        <span className={`size-2 rounded-full ${connected ? "bg-ok" : "bg-fld"}`} />
        {machine ? `${machine}${connected ? "" : " (away)"}` : "machine removed"}
        {!mine && ` · ${user}'s chat`}
        {chat.group_id && (
          <>
            ·
            <Link className="underline" to={`/groups/${chat.group_id}`}>
              Called in its group, and written to there
            </Link>
          </>
        )}
        {task && (
          <>
            ·
            <Link className="underline" to={`/projects/${chat.project_id}/tasks?task=${chat.task_id}`}>
              {task}
            </Link>
          </>
        )}
        {pull_url && (
          <>
            ·
            <a className="underline" href={pull_url} target="_blank" rel="noreferrer">
              Pull request #{pull_number}
            </a>
          </>
        )}
      </span>
      {hosts.length > 0 && (
        <span className="lbl flex w-full flex-wrap items-center gap-2">
          Commands may reach
          {hosts.map((host) => (
            <span key={host} className="flex items-center gap-1 rounded border border-line px-1.5 font-mono normal-case">
              {host}
              {mine && (
                <button type="button" aria-label={`Stop allowing ${host}`} onClick={() => forget.mutate(host)}>
                  ×
                </button>
              )}
            </span>
          ))}
        </span>
      )}
    </header>
  )
}

function Push({ chat, path }: Part) {
  const [message, setMessage] = useState(chat.title)
  const push = useChatChange(path, () => api("POST", `${path}/push`, { text: message }))
  return (
    <form
      className="flex flex-wrap items-center gap-2"
      onSubmit={(event) => {
        event.preventDefault()
        push.mutate()
      }}
    >
      {push.error && <p className="w-full text-acc">{push.error.message}</p>}
      <input
        aria-label="Commit message"
        className="min-w-0 flex-1 rounded-md border border-fld bg-bg px-3 py-2"
        value={message}
        onChange={(event) => setMessage(event.target.value)}
      />
      <button className="btn" disabled={push.isPending || !message.trim()}>
        {push.isPending ? "Pushing…" : chat.pull_url ? "Commit and push" : "Commit and open pull request"}
      </button>
    </form>
  )
}

function Ask({ chat, path }: Part) {
  const [text, setText] = useState("")
  const say = useChatChange(path, () => api("POST", `${path}/messages`, { text }).then(() => setText("")))
  const stop = useChatChange(path, () => api("POST", `${path}/stop`))
  const error = say.error ?? stop.error
  const name = agents[chat.agent].name
  return (
    <form
      className="composer"
      onSubmit={(event) => {
        event.preventDefault()
        if (!chat.running && text.trim()) say.mutate()
      }}
    >
      {error && <p className="w-full text-acc">{error.message}</p>}
      <textarea
        aria-label={`Ask ${name}`}
        placeholder={`Ask ${name}`}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && !event.shiftKey) {
            event.preventDefault()
            event.currentTarget.form?.requestSubmit()
          }
        }}
      />
      {chat.running ? (
        <button type="button" className="btn" disabled={stop.isPending} onClick={() => stop.mutate()}>
          Stop
        </button>
      ) : (
        <button className="btn btn-primary" disabled={!text.trim() || say.isPending}>
          Send
        </button>
      )}
    </form>
  )
}
