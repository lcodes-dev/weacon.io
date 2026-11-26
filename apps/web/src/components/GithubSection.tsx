import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useEffect, useState } from "react"
import { useSearchParams } from "react-router"
import { api, type Team } from "../api"
import { useGithub } from "../queries"

export function GithubSection({ team }: { team: Team }) {
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
