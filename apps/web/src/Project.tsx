import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { Link, useNavigate, useParams } from "react-router"
import { api, type Github, type Project, type Team } from "./api"

/** The page at `/projects/{id}`, where the id `new` makes a project. */
export function ProjectPage({ team }: { team: Team }) {
  const { id } = useParams()
  const projects = useQuery({
    queryKey: ["projects", team.id],
    queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`),
  })
  if (!projects.data) return null
  const project = projects.data.find((project) => String(project.id) === id)
  if (id !== "new" && !project) return <p className="page text-ink2">This project isn't in {team.name}.</p>
  return <Form key={id} team={team} project={project} />
}

function Form({ team, project }: { team: Team; project?: Project }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [name, setName] = useState(project?.name ?? "")
  const [repo, setRepo] = useState(project?.repo ?? "")
  const path = `/teams/${team.id}/projects`
  const github = useQuery({ queryKey: ["github", team.id], queryFn: () => api<Github>("GET", `/teams/${team.id}/github`) })
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["projects", team.id] })

  const save = useMutation({
    mutationFn: async () => {
      const fields = { name, repo: repo || undefined }
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
  const error = save.error ?? remove.error ?? github.error
  // A saved repository stays in the list even when the app no longer reaches it.
  const repos = new Set([...(github.data?.repos ?? []), ...(project?.repo ? [project.repo] : [])])

  return (
    <form
      className="page"
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
        <FieldControl render={<select />} value={repo} onChange={(event) => setRepo(event.target.value)}>
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
