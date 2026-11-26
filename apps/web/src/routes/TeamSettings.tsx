import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { api, type Team, type User } from "../api"
import { GithubSection } from "../components/GithubSection"
import { KeySection } from "../components/KeySection"
import { WriterSection } from "../components/WriterSection"
import { useTeamDetails } from "../queries"

export function TeamSettings({ team, user }: { team: Team; user: User }) {
  const queryClient = useQueryClient()
  const [name, setName] = useState(team.name)
  const [emails, setEmails] = useState("")
  const owner = team.role === "owner"
  const path = `/teams/${team.id}`
  const details = useTeamDetails(team)
  // The session first: after leaving, this team is gone and must not be fetched again.
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ["session"] })
    await queryClient.invalidateQueries({ queryKey: ["team", team.id] })
  }

  const rename = useMutation({ mutationFn: () => api("PUT", path, { name }), onSuccess: refresh })
  // Sending to an invited address again renews its link and its 7 days.
  const invite = useMutation({
    mutationFn: (emails: string[]) => api("POST", `${path}/invites`, { emails }),
    onSuccess: refresh,
  })
  const revoke = useMutation({
    mutationFn: (id: string) => api("DELETE", `${path}/invites/${id}`),
    onSuccess: refresh,
  })
  const remove = useMutation({
    mutationFn: (id: string) => api("DELETE", `${path}/members/${id}`),
    onSuccess: refresh,
  })
  const error = rename.error ?? invite.error ?? revoke.error ?? remove.error

  return (
    <div className="page">
      <h1>Team settings</h1>
      {owner ? (
        <form
          className="flex items-end gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            rename.mutate()
          }}
        >
          <Field className="flex-1">
            <FieldLabel>Name</FieldLabel>
            <Input value={name} onChange={(event) => setName(event.target.value)} required />
          </Field>
          <button className="btn btn-primary" disabled={rename.isPending}>
            Save
          </button>
        </form>
      ) : (
        <p className="font-medium">{team.name}</p>
      )}

      <h2>Members</h2>
      {details.data?.members.map((member) => (
        <div key={member.id} className="flex items-center gap-2">
          <div className="min-w-0 flex-1">
            <div className="truncate font-medium">{member.name}</div>
            <div className="truncate text-ink2">{member.email}</div>
          </div>
          {member.role === "owner" ? (
            <span className="text-ink2">owner</span>
          ) : (
            (owner || member.id === user.id) && (
              <button className="btn" onClick={() => remove.mutate(member.id)}>
                {member.id === user.id ? "Leave" : "Remove"}
              </button>
            )
          )}
        </div>
      ))}

      {details.data && details.data.invites.length > 0 && <h2>Invites</h2>}
      {details.data?.invites.map((invited) => (
        <div key={invited.id} className="flex items-center gap-2">
          <div className="min-w-0 flex-1">
            <div className="truncate font-medium">{invited.email}</div>
            <div className="text-ink2">{expiry(invited.expires_at)}</div>
          </div>
          {owner && (
            <>
              <button className="btn" disabled={invite.isPending} onClick={() => invite.mutate([invited.email])}>
                Resend
              </button>
              <button className="btn" onClick={() => revoke.mutate(invited.id)}>
                Revoke
              </button>
            </>
          )}
        </div>
      ))}

      {owner && (
        <form
          className="flex flex-col items-start gap-2"
          onSubmit={(event) => {
            event.preventDefault()
            const list = emails.split("\n").map((email) => email.trim())
            invite.mutate(list.filter(Boolean), { onSuccess: () => setEmails("") })
          }}
        >
          <h2>Invite people</h2>
          <Field className="self-stretch">
            <FieldLabel>Emails, one per line</FieldLabel>
            <FieldControl
              render={<textarea rows={4} />}
              value={emails}
              onChange={(event) => setEmails(event.target.value)}
              required
            />
          </Field>
          <button className="btn btn-primary" disabled={invite.isPending}>
            Send invites
          </button>
        </form>
      )}
      {invite.isSuccess && <p className="text-ok">Sent. An invite works for 7 days.</p>}
      {error && <p className="text-acc">{error.message}</p>}
      {owner && <GithubSection team={team} />}
      {owner && details.data && <KeySection team={team} saved={details.data.has_key} />}
      {owner && details.data && (
        <WriterSection key={`${details.data.writer?.kind} ${details.data.has_key}`} team={team} saved={details.data.writer} hasKey={details.data.has_key} />
      )}
    </div>
  )
}

function expiry(expiresAt: number) {
  const date = new Date(expiresAt * 1000)
  return date < new Date() ? "Expired" : `Expires ${date.toLocaleDateString()}`
}
