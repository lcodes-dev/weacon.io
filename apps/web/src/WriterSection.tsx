import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { api, type Team, type User, type Writer } from "./api"

const defaults = { claude_api: "claude-haiku-5-5", claude_code: "claude-haiku-5-5", codex: "gpt-5.6-luna" }

/** Who writes the team's docs, in team settings. */
export function WriterSection({ team, user, saved }: { team: Team; user: User; saved: Writer | null }) {
  const queryClient = useQueryClient()
  const [kind, setKind] = useState<Writer["kind"]>(saved?.kind ?? "claude_api")
  const [key, setKey] = useState("")
  const [model, setModel] = useState(saved?.model ?? "")
  const path = `/teams/${team.id}/writer`
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["team", team.id] })
  const save = useMutation({
    mutationFn: () => api("PUT", path, { kind, key: key || undefined, model: model || undefined }),
    onSuccess: refresh,
  })
  const forget = useMutation({ mutationFn: () => api("DELETE", path), onSuccess: refresh })
  const error = save.error ?? forget.error

  return (
    <form
      className="flex flex-col items-start gap-2"
      onSubmit={(event) => {
        event.preventDefault()
        save.mutate()
      }}
    >
      <h2>Docs writer</h2>
      <p className="text-ink2">
        {saved ? "The team's docs are written by the choice below." : "Choose who writes the docs of this team's projects."}
      </p>
      <Field className="self-stretch">
        <FieldLabel>Writer</FieldLabel>
        <FieldControl render={<select />} value={kind} onChange={(event) => setKind(event.target.value as Writer["kind"])}>
          <option value="claude_api">Claude, with an API key</option>
          {/* A program runs on the account of whoever runs the server. */}
          {user.is_owner && <option value="claude_code">Claude Code, signed in on the server's computer</option>}
          {user.is_owner && <option value="codex">Codex, signed in on the server's computer</option>}
        </FieldControl>
      </Field>
      {kind === "claude_api" && (
        <Field className="self-stretch">
          <FieldLabel>Anthropic API key</FieldLabel>
          <Input
            type="password"
            value={key}
            onChange={(event) => setKey(event.target.value)}
            placeholder={saved?.has_key ? "Unchanged" : ""}
          />
        </Field>
      )}
      <Field className="self-stretch">
        <FieldLabel>Model, or empty for the cheap default</FieldLabel>
        <Input value={model} onChange={(event) => setModel(event.target.value.trim())} placeholder={defaults[kind]} />
      </Field>
      <div className="flex gap-2">
        <button className="btn btn-primary" disabled={save.isPending}>
          Save
        </button>
        {saved && (
          <button type="button" className="btn" onClick={() => forget.mutate()}>
            Remove
          </button>
        )}
      </div>
      {save.isSuccess && <p className="text-ok">Saved.</p>}
      {error && <p className="text-acc">{error.message}</p>}
    </form>
  )
}
