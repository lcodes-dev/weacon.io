import { useMutation, useQueryClient } from "@tanstack/react-query"
import { Field, FieldControl, FieldLabel } from "@ui-sdk/react/field"
import { Input } from "@ui-sdk/react/input"
import { useState } from "react"
import { useNavigate } from "react-router"
import { api, type Team, type User } from "../api"
import { useProjects, useTeamDetails } from "../queries"

export function NewGroup({ team, user }: { team: Team; user: User }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const details = useTeamDetails(team)
  const projects = useProjects(team)
  const [members, setMembers] = useState<string[]>([])
  const [name, setName] = useState("")
  const [project, setProject] = useState("")
  const create = useMutation({
    mutationFn: () => api<{ id: string }>("POST", `/teams/${team.id}/groups`, { members, name, project_id: project || undefined }),
    onSuccess: async ({ id }) => {
      await queryClient.invalidateQueries({ queryKey: ["groups", team.id] })
      navigate(`/groups/${id}`)
    },
  })
  if (!details.data || !projects.data) return null
  const others = details.data.members.filter((member) => member.id !== user.id)
  return (
    <form
      className="page"
      onSubmit={(event) => {
        event.preventDefault()
        create.mutate()
      }}
    >
      <h1>New group</h1>
      <p className="text-ink2">Only the people in a group read it. With one other person it is a direct chat.</p>
      <fieldset className="flex flex-col gap-1">
        <legend className="pb-1 text-ink2">With</legend>
        {others.length === 0 && <p>Nobody else is in {team.name} yet.</p>}
        {others.map((member) => (
          <label key={member.id} className="flex items-center gap-2">
            <input
              type="checkbox"
              checked={members.includes(member.id)}
              onChange={(event) => setMembers(event.target.checked ? [...members, member.id] : members.filter((other) => other !== member.id))}
            />
            {member.name}
          </label>
        ))}
      </fieldset>
      <Field>
        <FieldLabel>Name</FieldLabel>
        <Input value={name} onChange={(event) => setName(event.target.value)} placeholder="Optional" />
      </Field>
      <Field>
        <FieldLabel>About the project</FieldLabel>
        <FieldControl render={<select />} value={project} onChange={(event) => setProject(event.target.value)}>
          <option value="">None</option>
          {projects.data.map((project) => (
            <option key={project.id} value={project.id}>
              {project.name}
            </option>
          ))}
        </FieldControl>
      </Field>
      <button className="btn btn-primary self-start" disabled={members.length === 0 || create.isPending}>
        Create
      </button>
      {create.error && <p className="text-acc">{create.error.message}</p>}
    </form>
  )
}
