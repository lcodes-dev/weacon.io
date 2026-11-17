import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { Link, useNavigate, useParams } from "react-router"
import { api, type Github, type Project, type RepoBranches, type Team } from "./api"
import { CodeMap } from "./CodeMap"

/** The page at `/projects/{id}`, where the id `new` makes a project. */
export function ProjectPage({ team }: { team: Team }) {
  const { id } = useParams()
  const projects = useQuery({
    queryKey: ["projects", team.id],
    queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`),
    // While a branch waits to be mapped, watch for the worker to finish.
    refetchInterval: (query) => (query.state.data?.some((project) => project.branches.some(waiting)) ? 3000 : false),
  })
  if (!projects.data) return null
  const project = projects.data.find((project) => String(project.id) === id)
  if (id !== "new" && !project) return <p className="page text-ink2">This project isn't in {team.name}.</p>
  return (
    <div className="page page-wide">
      <Form key={id} team={team} project={project} />
      {project && <CodeMap key={`map-${id}`} team={team} project={project} />}
    </div>
  )
}

function waiting(branch: Project["branches"][number]) {
  return !branch.error && (!branch.commit_sha || branch.commit_sha !== branch.head_sha)
}

function Form({ team, project }: { team: Team; project?: Project }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [name, setName] = useState(project?.name ?? "")
  const [repo, setRepo] = useState(project?.repo ?? "")
  // Unset means the repository's default branch.
  const [chosen, setChosen] = useState(project?.repo ? project.branches.map((branch) => branch.name) : undefined)
  const path = `/teams/${team.id}/projects`
  const github = useQuery({ queryKey: ["github", team.id], queryFn: () => api<Github>("GET", `/teams/${team.id}/github`) })
  const repoBranches = useQuery({
    queryKey: ["repo-branches", team.id, repo],
    queryFn: () => api<RepoBranches>("GET", `/teams/${team.id}/github/branches?repo=${repo}`),
    enabled: !!repo,
  })
  const branches = chosen ?? repoBranches.data?.branches.filter((name) => name === repoBranches.data.default) ?? []
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["projects", team.id] })

  const save = useMutation({
    mutationFn: async () => {
      const fields = { name, repo: repo || undefined, branches }
      if (!project) return api<Project>("POST", path, fields)
      await api("PUT", `${path}/${project.id}`, fields)
    },
    onSuccess: async (created) => {
      await refresh()
      if (created) navigate(`/projects/${created.id}`)
    },
  })
  const remove = useMutation({
    mutationFn: () => api("DELETE", `${path}/${project?.id}`),
    onSuccess: async () => {
      await refresh()
      navigate("/")
    },
  })
  const error = save.error ?? remove.error ?? github.error ?? repoBranches.error
  // A saved repository stays in the list even when the app no longer reaches it.
  const repos = new Set([...(github.data?.repos ?? []), ...(project?.repo ? [project.repo] : [])])

  return (
    <form
      className="flex max-w-[500px] flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault()
        save.mutate()
      }}
    >
      <h1>{project?.name ?? "New project"}</h1>
      <Field>
        <FieldLabel>Name</FieldLabel>
        <Input value={name} onChange={(event) => setName(event.target.value)} required />
      </Field>
      <Field>
        <FieldLabel>Repository</FieldLabel>
        <FieldControl
          render={<select />}
          value={repo}
          onChange={(event) => {
            setRepo(event.target.value)
            setChosen(undefined)
          }}
        >
          <option value="">No repository</option>
          {[...repos].map((repo) => (
            <option key={repo}>{repo}</option>
          ))}
        </FieldControl>
      </Field>
      {github.data?.account ? (
        <p className="text-ink2">From {github.data.account} on GitHub.</p>
      ) : (
        <Link to="/settings/team" className="text-ink2 underline">
          Set up GitHub in Team settings to pick a repository.
        </Link>
      )}
      {project?.repo && (
        <a href={`https://github.com/${project.repo}`} target="_blank" rel="noreferrer" className="underline">
          Open {project.repo} on GitHub
        </a>
      )}
      {repo && repoBranches.data && (
        <fieldset className="flex flex-col gap-1">
          <legend className="pb-1 text-ink2">Branches to map</legend>
          {repoBranches.data.branches.length === 0 && <p>This repository is empty.</p>}
          {[...new Set([...repoBranches.data.branches, ...branches])].map((name) => {
            const tracked = project?.repo === repo ? project.branches.find((branch) => branch.name === name) : undefined
            return (
              <label key={name} className="flex items-center gap-2">
                <input
                  type="checkbox"
                  checked={branches.includes(name)}
                  onChange={(event) =>
                    setChosen(event.target.checked ? [...branches, name] : branches.filter((other) => other !== name))
                  }
                />
                {name}
                {tracked && (
                  <span className={tracked.error ? "text-acc" : "text-ink2"}>
                    {tracked.error ?? (waiting(tracked) ? "being mapped…" : `mapped at ${tracked.commit_sha?.slice(0, 7)}`)}
                  </span>
                )}
              </label>
            )
          })}
        </fieldset>
      )}
      <div className="flex gap-2">
        <button className="btn btn-primary" disabled={save.isPending}>
          {project ? "Save" : "Create"}
        </button>
        {project && team.role === "owner" && (
          <button type="button" className="btn" onClick={() => remove.mutate()}>
            Delete project
          </button>
        )}
      </div>
      {save.isSuccess && project && <p className="text-ok">Saved.</p>}
      {error && <p className="text-acc">{error.message}</p>}
    </form>
  )
}
