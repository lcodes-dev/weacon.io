import { useQuery, useQueryClient } from "@tanstack/react-query"
import { useEffect, useRef, useState } from "react"
import { useParams } from "react-router"
import { agents, api, type OpenChat, type Team } from "../api"
import { ChatEvents, useChatChange } from "../components/ChatEvents"

type Part = { chat: OpenChat; path: string }

/** The page at `/chats/{id}`: what was asked, what the agent said and did, and a box to ask for more. */
export function ChatPage({ team }: { team: Team }) {
  const path = `/teams/${team.id}/chats/${useParams().id}`
  const queryClient = useQueryClient()
  const chat = useQuery({
    queryKey: ["chat", path],
    // Only what came after the last event this page has is asked for.
    queryFn: async () => {
      const had = queryClient.getQueryData<OpenChat>(["chat", path])?.events ?? []
      const now = await api<OpenChat>("GET", `${path}?after=${had.at(-1)?.id ?? ""}`)
      return { ...now, events: [...had, ...now.events] }
    },
    refetchInterval: (query) => (query.state.data?.running ? 1000 : 5000),
  })

  // The newest event stays in view as the agent works.
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
    <div className="mx-auto flex h-full max-w-3xl flex-col px-6">
      <Header chat={chat.data} path={path} />
      <div className="flex flex-1 flex-col gap-3 overflow-auto py-4">
        <ChatEvents chat={chat.data} path={path} />
        {running && <p className="animate-pulse text-ink2">{connected ? "Working…" : `Waiting for ${machine} to connect…`}</p>}
        <div ref={end} />
      </div>
      {mine && machine && changed && !running && <Push chat={chat.data} path={path} />}
      {mine && machine && <Ask chat={chat.data} path={path} />}
    </div>
  )
}

/** What the chat is, where it runs, and the hosts its commands may reach. */
function Header({ chat, path }: Part) {
  const { title, project, branch, agent, machine, connected, mine, user, hosts, pull_number, pull_url } = chat
  const forget = useChatChange(path, (host: string) => api("DELETE", `${path}/hosts/${host}`))
  return (
    <header className="flex flex-col gap-1 border-b border-line py-4">
      <h1 className="text-[17px] font-semibold">{title}</h1>
      <span className="lbl flex items-center gap-2">
        {project} · {branch} · {agents[agent].name} ·
        <span className={`size-2 rounded-full ${connected ? "bg-ok" : "bg-fld"}`} />
        {machine ? `${machine}${connected ? "" : " (away)"}` : "machine removed"}
        {!mine && ` · ${user}'s chat`}
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
        <span className="lbl flex flex-wrap items-center gap-2">
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

/** Commits what the chat changed and pushes it, which opens the chat's pull request the first time. */
function Push({ chat, path }: Part) {
  const [message, setMessage] = useState(chat.title)
  const push = useChatChange(path, () => api("POST", `${path}/push`, { text: message }))
  return (
    <form
      className="flex flex-wrap items-center gap-2 border-t border-line pt-4"
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

/** The box to ask the agent for more, which stops it while it works. */
function Ask({ chat, path }: Part) {
  const [text, setText] = useState("")
  const say = useChatChange(path, () => api("POST", `${path}/messages`, { text }).then(() => setText("")))
  const stop = useChatChange(path, () => api("POST", `${path}/stop`))
  const error = say.error ?? stop.error
  const name = agents[chat.agent].name
  return (
    <form
      className="flex flex-wrap items-end gap-2 border-t border-line py-4"
      onSubmit={(event) => {
        event.preventDefault()
        if (!chat.running && text.trim()) say.mutate()
      }}
    >
      {error && <p className="w-full text-acc">{error.message}</p>}
      <textarea
        aria-label={`Ask ${name}`}
        className="max-h-48 min-h-11 flex-1 rounded-md border border-fld bg-bg px-3 py-2"
        placeholder={`Ask ${name}`}
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          // Enter sends, and Shift with it makes a new line.
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
