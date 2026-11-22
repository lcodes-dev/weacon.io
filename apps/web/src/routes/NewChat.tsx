import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { useState } from "react"
import { Link, useNavigate } from "react-router"
import { agents, api, type Agent, type Machine, type Project, type Team } from "../api"

/** The page at `/chats/new`: what to ask, of which agent, on which project and machine. */
export function NewChat({ team }: { team: Team }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const projects = useQuery({ queryKey: ["projects", team.id], queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`) })
  const machines = useQuery({ queryKey: ["machines"], queryFn: () => api<Machine[]>("GET", "/machines") })
  const [chosen, setChosen] = useState<{ project?: string; branch?: string; agent?: Agent; machine?: string }>({})
  const [text, setText] = useState("")

  // An agent works in a copy of the repository, so a project without one has nothing to work on.
  const mapped = projects.data?.filter((project) => project.branches.length) ?? []
  const project = mapped.find((project) => project.id === chosen.project) ?? mapped[0]
  const branch = project?.branches.find((branch) => branch.name === chosen.branch) ?? project?.branches[0]
  const ready = (machine: Machine, agent: Agent) => machine.agents.some((found) => found.agent === agent && found.ready === "signed_in")
  const agent = chosen.agent ?? (["claude", "codex"] as const).find((agent) => machines.data?.some((machine) => ready(machine, agent))) ?? "claude"
  const able = machines.data?.filter((machine) => ready(machine, agent)) ?? []
  const machine = able.find((machine) => machine.id === chosen.machine) ?? able.find((machine) => machine.connected) ?? able[0]

  const start = useMutation({
    mutationFn: () =>
      api<{ id: string }>("POST", `/teams/${team.id}/projects/${project?.id}/chats`, {
        agent,
        machine_id: machine?.id,
        branch: branch?.name,
        text,
      }),
    onSuccess: ({ id }) => {
      queryClient.invalidateQueries({ queryKey: ["chats", team.id] })
      navigate(`/chats/${id}`)
    },
  })
  if (!projects.data || !machines.data) return null
  const select = "rounded-md border border-fld bg-bg px-2 py-1.5"
  return (
    <form
      className="page flex flex-col gap-3"
      onSubmit={(event) => {
        event.preventDefault()
        start.mutate()
      }}
    >
      <h1>New chat</h1>
      <p className="text-ink2">
        The agent works in a copy of the project's repository on your machine, on your own sign-in. It changes files there and nowhere else, and
        your team can read the chat.
      </p>
      {!project && <p className="text-ink2">No project of {team.name} has a repository with a mapped branch yet.</p>}
      {!machine && (
        <p className="text-ink2">
          None of your machines has {agents[agent].name} signed in. <Link to="/settings/machines">Look under Machines</Link>.
        </p>
      )}
      <div className="flex flex-wrap gap-3">
        <label className="flex flex-col gap-1">
          <span className="lbl">Project</span>
          <select className={select} value={project?.id ?? ""} onChange={(event) => setChosen({ ...chosen, project: event.target.value, branch: undefined })}>
            {mapped.map((project) => (
              <option key={project.id} value={project.id}>
                {project.name}
              </option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="lbl">Branch</span>
          <select className={select} value={branch?.name ?? ""} onChange={(event) => setChosen({ ...chosen, branch: event.target.value })}>
            {project?.branches.map((branch) => (
              <option key={branch.name}>{branch.name}</option>
            ))}
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="lbl">Agent</span>
          <select className={select} value={agent} onChange={(event) => setChosen({ ...chosen, agent: event.target.value as Agent, machine: undefined })}>
            <option value="claude">{agents.claude.name}</option>
            <option value="codex">{agents.codex.name}</option>
          </select>
        </label>
        <label className="flex flex-col gap-1">
          <span className="lbl">Machine</span>
          <select className={select} value={machine?.id ?? ""} onChange={(event) => setChosen({ ...chosen, machine: event.target.value })}>
            {able.map((machine) => (
              <option key={machine.id} value={machine.id}>
                {machine.name}
                {machine.connected ? "" : " (away)"}
              </option>
            ))}
          </select>
        </label>
      </div>
      <label className="flex flex-col gap-1">
        <span className="lbl">What should it do?</span>
        <textarea
          className="min-h-32 rounded-md border border-fld bg-bg px-3 py-2"
          value={text}
          onChange={(event) => setText(event.target.value)}
          placeholder="Make returns easy to find on mobile: a link in the footer and in the account menu."
        />
      </label>
      {start.error && <p className="text-acc">{start.error.message}</p>}
      <button className="btn btn-primary self-start" disabled={!project || !machine || !text.trim() || start.isPending}>
        Start
      </button>
    </form>
  )
}
