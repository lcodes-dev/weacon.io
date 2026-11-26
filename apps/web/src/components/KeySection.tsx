import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { api, type Team } from "../api"

export function KeySection({ team, saved }: { team: Team; saved: boolean }) {
  const queryClient = useQueryClient()
  const [key, setKey] = useState("")
  const path = `/teams/${team.id}/key`
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["team", team.id] })
  const save = useMutation({
    mutationFn: () => api("PUT", path, { key }).then(() => setKey("")),
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
      <h2>API key</h2>
      <p className="text-ink2">
        The team's own key for the Anthropic API. It pays for @weacon, the team's agent that any member calls in a group, and for the docs when the API
        writes them. A call to @weacon sends that group's last messages to Anthropic.
      </p>
      <Field className="self-stretch">
        <FieldLabel>Anthropic API key</FieldLabel>
        <Input type="password" value={key} onChange={(event) => setKey(event.target.value)} placeholder={saved ? "Saved. Write another to replace it" : ""} />
      </Field>
      <div className="flex gap-2">
        <button className="btn btn-primary" disabled={!key.trim() || save.isPending}>
          Save
        </button>
        {saved && (
          <button type="button" className="btn" onClick={() => forget.mutate()}>
            Remove
          </button>
        )}
      </div>
      {error && <p className="text-acc">{error.message}</p>}
    </form>
  )
}
