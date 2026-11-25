import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { FileTextIcon, PencilIcon, TerminalIcon, WrenchIcon } from "lucide-react"
import Markdown from "markdown-to-jsx"
import type { ReactNode } from "react"
import { Link } from "react-router"
import { api, type ChatEvent, type OpenChat } from "../api"
import { safeUrl } from "./DocPages"

const icons = { read: FileTextIcon, edit: PencilIcon, command: TerminalIcon, other: WrenchIcon }

/** A request that changes the chat at `path`, after which the chat is read again. */
export function useChatChange<T = void>(path: string, change: (input: T) => Promise<unknown>) {
  const queryClient = useQueryClient()
  return useMutation({ mutationFn: change, onSuccess: () => queryClient.invalidateQueries({ queryKey: ["chat", path] }) })
}

/** What was asked in a chat and what its agent said and did, oldest first. */
export function ChatEvents({ chat, path }: { chat: OpenChat; path: string }) {
  const { events, hosts, mine, machine, running, user, project_id } = chat
  // The chat's files as its last finished turn left them, which is what the diff is of.
  const done = events.filter((event) => event.kind === "done").at(-1)
  const diff = useQuery({
    queryKey: ["chat-diff", path, done?.id],
    queryFn: () => api<{ diff: string }>("GET", `${path}/diff`),
    enabled: done !== undefined && done.changed.length > 0,
  })
  const allow = useChatChange(path, (host: string) => api("POST", `${path}/hosts`, { host }))
  // A host is allowed where it was last wanted, once the turn that wanted it has ended.
  const wanted = new Map(events.flatMap((event) => (event.kind === "wants" ? [[event.host, event]] : [])))
  const allowable = (event: ChatEvent) =>
    event.kind === "wants" && mine && machine && !running && !hosts.includes(event.host) && wanted.get(event.host) === event
  return (
    <>
      {events.map((event) => (
        <div key={event.id} className="contents">
          <Event event={event} user={user} project={project_id} diff={event === done ? diff.data?.diff : undefined} />
          {event.kind === "wants" && allowable(event) && (
            <div className="flex flex-wrap items-center gap-2">
              <button type="button" className="btn" disabled={allow.isPending} onClick={() => allow.mutate(event.host)}>
                Allow and try again
              </button>
              <span className="lbl normal-case">Commands in this chat can then send data to this host.</span>
            </div>
          )}
        </div>
      ))}
      {allow.error && <p className="text-acc">{allow.error.message}</p>}
    </>
  )
}

/** Markdown someone or an agent wrote, with nothing in it fetched from another host or run. */
export function Said({ text }: { text: string }) {
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
        {text}
      </Markdown>
    </div>
  )
}

function Event({ event, user, project, diff }: { event: ChatEvent; user: string; project: string; diff?: string }) {
  switch (event.kind) {
    case "asked":
      return (
        <div className="flex flex-col gap-1 self-end rounded-lg bg-sur px-3 py-2">
          <span className="lbl">{user}</span>
          <p className="whitespace-pre-wrap">{event.text}</p>
        </div>
      )
    case "said":
      return <Said text={event.text} />
    case "did": {
      const Icon = icons[event.deed]
      return (
        <div className="flex items-center gap-2 text-ink2">
          <Icon className="size-3.5 shrink-0" aria-label={event.deed} />
          <code className="truncate font-mono text-[12px]">{event.what}</code>
        </div>
      )
    }
    case "failed":
      return <p className="text-acc">{event.why}</p>
    case "wants":
      return (
        <p className="text-ink2">
          A command was refused <code className="font-mono text-[12px]">{event.host}</code>
        </p>
      )
    case "allowed":
      return (
        <p className="lbl normal-case">
          <code className="font-mono">{event.host}</code> is allowed in this chat
        </p>
      )
    case "task":
      return (
        <p className="lbl normal-case">
          {event.made ? "Made the task" : "Changed the task"}{" "}
          <Link className="underline" to={`/projects/${project}/tasks?task=${event.task_id}`}>
            {event.name}
          </Link>
        </p>
      )
    case "proposal":
      return (
        <p className="lbl normal-case">
          {event.new ? "Proposed the new page" : "Proposed a change to"} “{event.title}” in the{" "}
          <Link className="underline" to={`/projects/${project}/docs`}>
            docs
          </Link>
        </p>
      )
    case "done":
      return <Changed files={event.changed} diff={diff} />
  }
}

type File = { path: string; added: number; removed: number }

/** The files a turn left changed. With the chat's `diff`, each opens to its changed lines. */
function Changed({ files, diff }: { files: File[]; diff?: string }) {
  if (!files.length) return <p className="lbl">Done · no file changed</p>
  return (
    <div className="flex flex-col gap-1 rounded-lg border border-line p-3 font-mono text-[12px]">
      <span className="lbl">
        {files.length} {files.length === 1 ? "file" : "files"} changed on the machine
      </span>
      {files.map((file) => {
        const counts = (
          <>
            <span className="mr-auto truncate">{file.path}</span>
            <span className="text-ok">+{file.added}</span>
            <span className="text-acc">−{file.removed}</span>
          </>
        )
        if (!diff)
          return (
            <div key={file.path} className="flex gap-3">
              {counts}
            </div>
          )
        return (
          <details key={file.path}>
            <summary className="flex cursor-pointer gap-3">{counts}</summary>
            <pre className="mt-1 overflow-auto rounded-md border border-line py-1 leading-[1.6]">
              {lines(diff, file.path).map((line, at) => (
                <div key={at} className={`px-2 ${colors[line[0]] ?? ""}`}>
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

/** How a line of a diff is shown, by the character it starts with. */
const colors: Record<string, string> = { "+": "bg-ok/10 text-ok", "-": "bg-acc/10 text-acc", "@": "text-ink2" }

/** One file's part of a diff, without git's own lines above the first changed place. */
function lines(diff: string, path: string) {
  const file = diff.split(/^diff --git /m).find((file) => file.split("\n", 1)[0].endsWith(` b/${path}`))
  const all = file?.split("\n").slice(1, -1) ?? []
  const first = all.findIndex((line) => line.startsWith("@@"))
  // A file that isn't text has no changed place, only a line saying so.
  return first < 0 ? all.slice(-1) : all.slice(first)
}
