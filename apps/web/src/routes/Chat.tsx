import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { FileTextIcon, PencilIcon, TerminalIcon, WrenchIcon } from "lucide-react"
import Markdown from "markdown-to-jsx"
import { useEffect, useRef, useState, type ReactNode } from "react"
import { useParams } from "react-router"
import { agents, api, type ChatEvent, type OpenChat, type Team } from "../api"
import { safeUrl } from "../components/DocPages"

const icons = { read: FileTextIcon, edit: PencilIcon, command: TerminalIcon, other: WrenchIcon }

/** The page at `/chats/{id}`: what was asked, what the agent said and did, and a box to ask for more. */
export function ChatPage({ team }: { team: Team }) {
  const { id } = useParams()
  const queryClient = useQueryClient()
  const key = ["chat", team.id, id]
  const chat = useQuery({
    queryKey: key,
    // Only what came after the last event this page has is asked for.
    queryFn: async () => {
      const had = queryClient.getQueryData<OpenChat>(key)?.events ?? []
      const after = had.at(-1)?.id ?? ""
      const now = await api<OpenChat>("GET", `/teams/${team.id}/chats/${id}?after=${after}`)
      return { ...now, events: [...had, ...now.events] }
    },
    refetchInterval: (query) => (query.state.data?.running ? 1000 : 5000),
  })
  const [text, setText] = useState("")
  const refresh = () => queryClient.invalidateQueries({ queryKey: key })
  const say = useMutation({
    mutationFn: () => api("POST", `/teams/${team.id}/chats/${id}/messages`, { text }),
    onSuccess: () => {
      setText("")
      return refresh()
    },
  })
  const stop = useMutation({ mutationFn: () => api("POST", `/teams/${team.id}/chats/${id}/stop`), onSuccess: refresh })
  const [message, setMessage] = useState<string>()
  const push = useMutation({
    mutationFn: (text: string) => api("POST", `/teams/${team.id}/chats/${id}/push`, { text }),
    onSuccess: refresh,
  })
  // The chat's files as its last finished turn left them, which is what the diff is of.
  const done = chat.data?.events.filter((event) => event.kind === "done").at(-1)
  const changed = done?.kind === "done" && done.changed.length > 0
  const diff = useQuery({
    queryKey: ["chat-diff", team.id, id, done?.id],
    queryFn: () => api<{ diff: string }>("GET", `/teams/${team.id}/chats/${id}/diff`),
    enabled: changed,
  })

  // The newest event stays in view as the agent works.
  const end = useRef<HTMLDivElement>(null)
  const told = chat.data?.events.length
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" })
  }, [told])

  if (chat.error) return <p className="page text-ink2">{chat.error.message}</p>
  if (!chat.data) return null
  const { title, project, branch, agent, machine, connected, running, mine, user, events, pull_number, pull_url } = chat.data
  const error = say.error ?? stop.error ?? push.error
  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col px-6">
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
      </header>
      <div className="flex flex-1 flex-col gap-3 overflow-auto py-4">
        {events.map((event) => (
          <Event key={event.id} event={event} user={user} diff={event === done ? diff.data?.diff : undefined} />
        ))}
        {running && <p className="animate-pulse text-ink2">{connected ? "Working…" : `Waiting for ${machine} to connect…`}</p>}
        <div ref={end} />
      </div>
      {error && <p className="text-acc">{error.message}</p>}
      {mine && machine && changed && !running && (
        <form
          className="flex items-center gap-2 border-t border-line pt-4"
          onSubmit={(event) => {
            event.preventDefault()
            push.mutate(message ?? title)
          }}
        >
          <input
            aria-label="Commit message"
            className="min-w-0 flex-1 rounded-md border border-fld bg-bg px-3 py-2"
            value={message ?? title}
            onChange={(event) => setMessage(event.target.value)}
          />
          <button className="btn" disabled={push.isPending || !(message ?? title).trim()}>
            {push.isPending ? "Pushing…" : pull_url ? "Commit and push" : "Commit and open pull request"}
          </button>
        </form>
      )}
      {mine && machine && (
        <form
          className="flex items-end gap-2 border-t border-line py-4"
          onSubmit={(event) => {
            event.preventDefault()
            say.mutate()
          }}
        >
          <textarea
            aria-label={`Ask ${agents[agent].name}`}
            className="max-h-48 min-h-11 flex-1 rounded-md border border-fld bg-bg px-3 py-2"
            placeholder={`Ask ${agents[agent].name}`}
            value={text}
            onChange={(event) => setText(event.target.value)}
            onKeyDown={(event) => {
              // Enter sends, and Shift with it makes a new line.
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault()
                if (!running && text.trim()) say.mutate()
              }
            }}
          />
          {running ? (
            <button type="button" className="btn" disabled={stop.isPending} onClick={() => stop.mutate()}>
              Stop
            </button>
          ) : (
            <button className="btn btn-primary" disabled={!text.trim() || say.isPending}>
              Send
            </button>
          )}
        </form>
      )}
    </div>
  )
}

/** One file's part of a diff, without git's own lines above the first changed place. */
function lines(diff: string, path: string) {
  const file = diff.split(/^diff --git /m).find((file) => file.split("\n", 1)[0].endsWith(` b/${path}`))
  const all = file?.split("\n").slice(1, -1) ?? []
  const first = all.findIndex((line) => line.startsWith("@@"))
  // A file that isn't text has no changed place, only a line saying so.
  return first < 0 ? all.slice(-1) : all.slice(first)
}

function Event({ event, user, diff }: { event: ChatEvent; user: string; diff?: string }) {
  if (event.kind === "asked")
    return (
      <div className="flex flex-col gap-1 self-end rounded-lg bg-sur px-3 py-2">
        <span className="lbl">{user}</span>
        <p className="whitespace-pre-wrap">{event.text}</p>
      </div>
    )
  if (event.kind === "said")
    return (
      <div className="prose">
        {/* An image on another host would be fetched on sight, so it is shown as a link. */}
        <Markdown
          options={{
            disableParsingRawHTML: true,
            forceBlock: true,
            sanitizer: safeUrl,
            overrides: {
              img: ({ src, alt }: { src?: string; alt?: string }) => <a href={src}>{alt || src}</a>,
              // An agent links to files in its folder, which aren't pages here.
              a: ({ href, children }: { href?: string; children?: ReactNode }) =>
                /^(https?|mailto):/.test(href ?? "") ? <a href={href}>{children}</a> : <>{children}</>,
            },
          }}
        >
          {event.text}
        </Markdown>
      </div>
    )
  if (event.kind === "did") {
    const Icon = icons[event.deed]
    return (
      <div className="flex items-center gap-2 text-ink2">
        <Icon className="size-3.5 shrink-0" aria-label={event.deed} />
        <code className="truncate font-mono text-[12px]">{event.what}</code>
      </div>
    )
  }
  if (event.kind === "failed") return <p className="text-acc">{event.why}</p>
  if (!event.changed.length) return <p className="lbl">Done · no file changed</p>
  return (
    <div className="flex flex-col gap-1 rounded-lg border border-line p-3">
      <span className="lbl">
        {event.changed.length} {event.changed.length === 1 ? "file" : "files"} changed on the machine
      </span>
      {event.changed.map((file) => {
        const counts = (
          <>
            <span className="mr-auto truncate">{file.path}</span>
            <span className="text-ok">+{file.added}</span>
            <span className="text-acc">−{file.removed}</span>
          </>
        )
        // Only the newest turn's files open: the diff is of the chat as it is now.
        if (!diff) return <div key={file.path} className="flex gap-3 font-mono text-[12px]">{counts}</div>
        return (
          <details key={file.path} className="font-mono text-[12px]">
            <summary className="flex cursor-pointer gap-3">{counts}</summary>
            <pre className="mt-1 overflow-auto rounded-md border border-line py-1 leading-[1.6]">
              {lines(diff, file.path).map((line, at) => (
                <div
                  key={at}
                  className={`px-2 ${line[0] === "+" ? "bg-ok/10 text-ok" : line[0] === "-" ? "bg-acc/10 text-acc" : line[0] === "@" ? "text-ink2" : ""}`}
                >
                  {line || " "}
                </div>
              ))}
            </pre>
          </details>
        )
      })}
      {diff === "" && <span className="lbl">The changes are too long to show here.</span>}
    </div>
  )
}
