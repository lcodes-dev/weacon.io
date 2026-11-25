import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useEffect, useRef, useState } from "react"
import { Link, useNavigate, useParams } from "react-router"
import { api, groupName, type OpenGroup, type Project, type Team, type TeamDetails, type User } from "../api"
import { Said } from "../components/ChatEvents"

type Part = { group: OpenGroup; path: string; team: Team }

/** The page at `/groups/{id}`: what its members wrote to each other, and a box to write in. */
export function GroupPage({ team, user }: { team: Team; user: User }) {
  const path = `/teams/${team.id}/groups/${useParams().id}`
  const queryClient = useQueryClient()
  const group = useQuery({
    queryKey: ["group", path],
    // Only what came after the last message this page has is asked for.
    queryFn: async () => {
      const had = queryClient.getQueryData<OpenGroup>(["group", path])?.messages ?? []
      const now = await api<OpenGroup>("GET", `${path}?after=${had.at(-1)?.id ?? ""}`)
      return { ...now, messages: [...had, ...now.messages] }
    },
    refetchInterval: 2000,
  })

  // The newest message stays in view.
  const end = useRef<HTMLDivElement>(null)
  const written = group.data?.messages.length
  useEffect(() => {
    end.current?.scrollIntoView({ block: "end" })
  }, [written])

  if (group.error) return <p className="page text-ink2">{group.error.message}</p>
  if (!group.data) return null
  const { members, project, project_id, messages } = group.data
  return (
    <div className="mx-auto flex h-full max-w-3xl flex-col px-6">
      <header className="flex flex-col gap-1 border-b border-line py-4">
        <h1 className="text-[17px] font-semibold">{groupName(group.data, user.id)}</h1>
        <span className="lbl flex flex-wrap items-center gap-2 normal-case">
          {members.map((member) => member.name).join(", ")}
          {project_id && (
            <>
              ·
              <Link className="underline" to={`/projects/${project_id}`}>
                {project}
              </Link>
            </>
          )}
        </span>
        <Settings key={group.data.id} group={group.data} path={path} team={team} />
      </header>
      <div className="flex flex-1 flex-col gap-3 overflow-auto py-4">
        {messages.length === 0 && <p className="text-ink2">Nothing written yet.</p>}
        {messages.map((message) => (
          <div key={message.id} className="flex flex-col gap-0.5">
            <span className="lbl normal-case">
              <span className="font-semibold text-ink">{message.user ?? "Someone who left"}</span>{" "}
              {new Date(message.created_at * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" })}
            </span>
            <Said text={message.text} />
          </div>
        ))}
        <div ref={end} />
      </div>
      <Write group={group.data} path={path} team={team} />
    </div>
  )
}

/** A request that changes the group at `path`, after which it and the sidebar's list are read again. */
function useGroupChange<T = void>({ path, team }: Pick<Part, "path" | "team">, change: (input: T) => Promise<unknown>) {
  const queryClient = useQueryClient()
  return useMutation({
    mutationFn: change,
    onSuccess: () => Promise.all([queryClient.invalidateQueries({ queryKey: ["group", path] }), queryClient.invalidateQueries({ queryKey: ["groups", team.id] })]),
  })
}

/** Its name, its project while it has none, who else joins, and leaving. */
function Settings({ group, path, team }: Part) {
  const navigate = useNavigate()
  const [name, setName] = useState(group.name ?? "")
  const details = useQuery({ queryKey: ["team", team.id], queryFn: () => api<TeamDetails>("GET", `/teams/${team.id}`) })
  const projects = useQuery({ queryKey: ["projects", team.id], queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`) })
  const change = useGroupChange({ path, team }, (project_id?: string) => api("PUT", path, { name, project_id }))
  const add = useGroupChange({ path, team }, (user_id: string) => api("POST", `${path}/members`, { user_id }))
  const leave = useGroupChange({ path, team }, () => api("DELETE", `${path}/members/me`).then(() => navigate("/")))
  const error = change.error ?? add.error ?? leave.error
  const outside = details.data?.members.filter((member) => !group.members.some((inside) => inside.id === member.id)) ?? []
  return (
    <details className="text-[13px]">
      <summary className="cursor-pointer text-ink2">Group settings</summary>
      <div className="flex flex-wrap items-end gap-3 pt-2">
        <form
          className="flex items-end gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            change.mutate(undefined)
          }}
        >
          <Field>
            <FieldLabel>Name</FieldLabel>
            <Input value={name} onChange={(event) => setName(event.target.value)} placeholder="None" />
          </Field>
          {name !== (group.name ?? "") && <button className="btn">Save</button>}
        </form>
        {!group.project_id && (
          <Field>
            <FieldLabel>About the project</FieldLabel>
            <FieldControl render={<select />} value="" onChange={(event) => change.mutate(event.target.value)}>
              <option value="">None</option>
              {projects.data?.map((project) => (
                <option key={project.id} value={project.id}>
                  {project.name}
                </option>
              ))}
            </FieldControl>
          </Field>
        )}
        {outside.length > 0 && (
          <Field>
            <FieldLabel>Add someone</FieldLabel>
            <FieldControl render={<select />} value="" onChange={(event) => add.mutate(event.target.value)}>
              <option value="">Pick a member</option>
              {outside.map((member) => (
                <option key={member.id} value={member.id}>
                  {member.name}
                </option>
              ))}
            </FieldControl>
          </Field>
        )}
        <button type="button" className="btn" disabled={leave.isPending} onClick={() => leave.mutate()}>
          Leave
        </button>
      </div>
      {error && <p className="pt-2 text-acc">{error.message}</p>}
    </details>
  )
}

function Write({ path, team }: Part) {
  const [text, setText] = useState("")
  const write = useGroupChange({ path, team }, () => api("POST", `${path}/messages`, { text }).then(() => setText("")))
  return (
    <form
      className="flex flex-wrap items-end gap-2 border-t border-line py-4"
      onSubmit={(event) => {
        event.preventDefault()
        if (text.trim()) write.mutate()
      }}
    >
      {write.error && <p className="w-full text-acc">{write.error.message}</p>}
      <textarea
        aria-label="Write to the group"
        className="max-h-48 min-h-11 flex-1 rounded-md border border-fld bg-bg px-3 py-2"
        placeholder="Write to the group"
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
      <button className="btn btn-primary" disabled={!text.trim() || write.isPending}>
        Send
      </button>
    </form>
  )
}
