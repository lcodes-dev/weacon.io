import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useEffect, useState } from "react"
import { Link, useSearchParams } from "react-router"
import { api, type Team, type User, type Writer } from "../api"
import { useGithub, useTeamDetails } from "../queries"

export function TeamSettings({ team, user }: { team: Team; user: User }) {
  const queryClient = useQueryClient()
  const [name, setName] = useState(team.name)
  const [emails, setEmails] = useState("")
  const owner = team.role === "owner"
  const path = `/teams/${team.id}`
  const details = useTeamDetails(team)
  const refresh = async () => {
    await queryClient.invalidateQueries({ queryKey: ["session"] })
    await queryClient.invalidateQueries({ queryKey: ["team", team.id] })
  }

  const rename = useMutation({ mutationFn: () => api("PUT", path, { name }), onSuccess: refresh })
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

const defaults = { claude_api: "claude-haiku-5-5", claude_code: "claude-haiku-5-5", codex: "gpt-5.6-luna" }

function WriterSection({ team, saved, hasKey }: { team: Team; saved: Writer | null; hasKey: boolean }) {
  const queryClient = useQueryClient()
  const [kind, setKind] = useState<Writer["kind"]>(saved?.kind ?? (hasKey ? "claude_api" : "claude_code"))
  const [model, setModel] = useState(saved?.model ?? "")
  const path = `/teams/${team.id}/writer`
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["team", team.id] })
  const save = useMutation({
    mutationFn: () => api("PUT", path, { kind, model: model || undefined }),
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
          <option value="claude_api" disabled={!hasKey}>
            Claude, on the server, with the team's API key
          </option>
          <option value="claude_code">Claude Code, on a machine of whoever starts a run</option>
          <option value="codex">Codex, on a machine of whoever starts a run</option>
        </FieldControl>
      </Field>
      {kind !== "claude_api" && (
        <p className="text-ink2">
          A run, a request or a page's check then uses a machine of the person who causes it, on their own subscription. Each member pairs their own
          under <Link to="/settings/machines" className="underline">Machines</Link>.
        </p>
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

function KeySection({ team, saved }: { team: Team; saved: boolean }) {
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

function GithubSection({ team }: { team: Team }) {
  const queryClient = useQueryClient()
  const [params, setParams] = useSearchParams()
  const [organization, setOrganization] = useState("")
  const [state] = useState(() => `${team.id}-${crypto.randomUUID()}`)
  const path = `/teams/${team.id}/github`
  const github = useGithub(team)
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["github", team.id] })
  const register = useMutation({ mutationFn: (code: string) => api("POST", path, { code }), onSuccess: refresh })
  const forget = useMutation({ mutationFn: () => api("DELETE", path), onSuccess: refresh })

  useEffect(() => {
    const code = params.get("code")
    if (!code) return
    const started = sessionStorage.getItem("github_state")
    if (started?.startsWith(`${team.id}-`) && started === params.get("state")) register.mutate(code)
    sessionStorage.removeItem("github_state")
    setParams({}, { replace: true })
  }, [])

  const origin = location.origin
  const manifest = {
    name: `Weacon ${team.name}`.slice(0, 34),
    url: origin,
    redirect_url: `${origin}/settings/team`,
    setup_url: `${origin}/settings/team`,
    public: false,
    default_permissions: { contents: "write", pull_requests: "write" },
  }
  const owner = organization ? `organizations/${organization}/` : ""
  const error = github.error ?? register.error ?? forget.error
  const app = github.data?.app

  return (
    <>
      <h2>GitHub</h2>
      {app ? (
        <>
          <p>
            {github.data?.account
              ? `The app ${app} reaches ${github.data.repos.length} repositories of ${github.data.account}.`
              : `The app ${app} is registered. Choose which repositories it may read.`}
          </p>
          <div className="flex gap-2">
            <a className="btn btn-primary" href={`https://github.com/apps/${app}/installations/new`}>
              Choose repositories
            </a>
            <button className="btn" onClick={() => forget.mutate()}>
              Remove GitHub
            </button>
          </div>
          <p className="text-ink2">Removing it here keeps the app on GitHub until you delete it there.</p>
        </>
      ) : (
        <form
          method="post"
          action={`https://github.com/${owner}settings/apps/new?state=${state}`}
          className="flex flex-col items-start gap-2"
          onSubmit={() => sessionStorage.setItem("github_state", state)}
        >
          <p className="text-ink2">
            Projects pick their repository from the ones this team's GitHub App can read. Setting it up takes two steps
            on GitHub: registering the app, then choosing repositories.
          </p>
          <input type="hidden" name="manifest" value={JSON.stringify(manifest)} />
          <Field className="self-stretch">
            <FieldLabel>GitHub organization, or empty for your own account</FieldLabel>
            <Input value={organization} onChange={(event) => setOrganization(event.target.value.trim())} />
          </Field>
          <button className="btn btn-primary" disabled={register.isPending}>
            Set up GitHub
          </button>
        </form>
      )}
      {error && <p className="text-acc">{error.message}</p>}
    </>
  )
}
