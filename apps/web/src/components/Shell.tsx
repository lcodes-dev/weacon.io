import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { OverlayHost } from "@ui-sdk/react/overlays"
import { lazy, Suspense, useState } from "react"
import { Link, Route, Routes } from "react-router"
import { agents, api, groupName, type Chat, type Group, type Machine, type MailConfig, type Project, type Team, type User } from "../api"
import { useLive } from "../live"
import { ChatPage } from "../routes/Chat"
import { GroupPage } from "../routes/Group"
import { Machines } from "../routes/Machines"
import { MailSettings } from "../routes/MailSettings"
import { NewChat } from "../routes/NewChat"
import { NewGroup } from "../routes/NewGroup"
import { ProjectPage } from "../routes/Project"
import { TasksPage } from "../routes/ProjectTasks"
import { TeamSettings } from "../routes/TeamSettings"

// The docs page is the largest, so it is fetched when first opened.
const DocsPage = lazy(() => import("../routes/ProjectDocs").then((module) => ({ default: module.DocsPage })))

export function Shell({ user }: { user: User }) {
  const [teamId, setTeamId] = useState(() => localStorage.getItem("team"))
  const team = user.teams.find((team) => team.id === teamId) ?? user.teams[0]
  const live = useLive()
  const openTeam = (id: string) => {
    localStorage.setItem("team", id)
    setTeamId(id)
  }
  return (
    <div className="flex h-screen">
      <aside className="flex w-[272px] shrink-0 flex-col gap-3 border-r border-line bg-side p-3">
        <div className="flex items-center justify-between px-1">
          <Link to="/" className="text-[15px] font-semibold">
            weacon
          </Link>
          <ServerStatus up={live} />
        </div>
        <Link to="/chats/new" className="rounded-md bg-pri py-1.5 text-center font-medium text-pri-text hover:bg-pri-hover">
          New chat
        </Link>
        <nav className="flex flex-col">
          <a className="rounded-md px-2 py-1.5 hover:bg-hov">Tasks</a>
          <a className="rounded-md px-2 py-1.5 hover:bg-hov">Docs</a>
        </nav>
        <Projects team={team} />
        <Groups team={team} user={user} />
        <Chats team={team} user={user} />
        <YourMachines />
        <div className="mt-auto flex flex-col gap-2">
          {user.is_owner && <OwnerMail />}
          <Account user={user} team={team} openTeam={openTeam} />
        </div>
      </aside>
      <main className="flex-1 overflow-auto">
        <Routes>
          <Route
            path="/projects/:id/docs"
            element={
              <Suspense>
                <DocsPage team={team} />
              </Suspense>
            }
          />
          <Route path="/projects/:id/tasks" element={<TasksPage team={team} user={user} />} />
          <Route path="/projects/:id" element={<ProjectPage team={team} />} />
          <Route path="/groups/new" element={<NewGroup team={team} user={user} />} />
          <Route path="/groups/:id" element={<GroupPage team={team} user={user} />} />
          <Route path="/chats/new" element={<NewChat team={team} />} />
          <Route path="/chats/:id" element={<ChatPage team={team} />} />
          <Route path="/settings/team" element={<TeamSettings key={team.id} team={team} user={user} />} />
          <Route path="/settings/machines" element={<Machines />} />
          {user.is_owner && <Route path="/settings/mail" element={<MailSettings />} />}
          <Route path="*" element={<p className="flex h-full items-center justify-center text-ink2">No chat open</p>} />
        </Routes>
      </main>
      <OverlayHost />
    </div>
  )
}

function Projects({ team }: { team: Team }) {
  const projects = useQuery({
    queryKey: ["projects", team.id],
    queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`),
  })
  return (
    <nav className="flex flex-col">
      <h2 className="px-2 pt-2 pb-1 font-mono text-[10px] tracking-wider text-ink3 uppercase">Projects</h2>
      {projects.data?.map((project) => (
        <Link key={project.id} to={`/projects/${project.id}`} className="truncate rounded-md px-2 py-1.5 hover:bg-hov">
          {project.name}
        </Link>
      ))}
      <Link to="/projects/new" className="rounded-md px-2 py-1.5 text-ink2 hover:bg-hov">
        New project
      </Link>
    </nav>
  )
}

/** The groups you are in, the one written in last first, each with a dot while it has something unread. */
function Groups({ team, user }: { team: Team; user: User }) {
  const groups = useQuery({
    queryKey: ["groups", team.id],
    queryFn: () => api<Group[]>("GET", `/teams/${team.id}/groups`),
  })
  return (
    <nav className="flex flex-col">
      <h2 className="px-2 pt-2 pb-1 font-mono text-[10px] tracking-wider text-ink3 uppercase">Groups</h2>
      {groups.data?.map((group) => (
        <Link key={group.id} to={`/groups/${group.id}`} className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-hov">
          <span className={`size-2 shrink-0 rounded-full ${group.unread ? "bg-pri" : "bg-transparent"}`} aria-label={group.unread ? "Unread" : undefined} />
          <span className={`truncate ${group.unread ? "font-semibold" : ""}`}>{groupName(group, user.id)}</span>
        </Link>
      ))}
      <Link to="/groups/new" className="rounded-md px-2 py-1.5 text-ink2 hover:bg-hov">
        New group
      </Link>
    </nav>
  )
}

/** The team's chats: your own, then your teammates', which you can read. */
function Chats({ team, user }: { team: Team; user: User }) {
  const chats = useQuery({
    queryKey: ["chats", team.id],
    queryFn: () => api<Chat[]>("GET", `/teams/${team.id}/chats`),
  })
  const list = (title: string, chats: Chat[], whose: boolean) => (
    <nav className="flex flex-col">
      <h2 className="px-2 pt-2 pb-1 font-mono text-[10px] tracking-wider text-ink3 uppercase">{title}</h2>
      {chats.map((chat) => (
        <Link key={chat.id} to={`/chats/${chat.id}`} className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-hov">
          <span className={`size-2 shrink-0 rounded-full ${chat.running ? "animate-pulse bg-ok" : "bg-fld"}`} />
          <span className="flex min-w-0 flex-col">
            <span className="truncate">{chat.title}</span>
            <span className="truncate text-[12px] text-ink2">
              {whose && `${chat.user} · `}
              {chat.project} · {agents[chat.agent].name}
            </span>
          </span>
        </Link>
      ))}
    </nav>
  )
  const theirs = chats.data?.filter((chat) => chat.user_id !== user.id) ?? []
  return (
    <>
      {list("Your chats", chats.data?.filter((chat) => chat.user_id === user.id) ?? [], false)}
      {theirs.length > 0 && list("Your team's chats", theirs, true)}
    </>
  )
}

/** Your machines, each with whether its core is connected and the agents ready on it. */
function YourMachines() {
  const machines = useQuery({ queryKey: ["machines"], queryFn: () => api<Machine[]>("GET", "/machines") })
  return (
    <nav className="flex flex-col">
      <h2 className="px-2 pt-2 pb-1 font-mono text-[10px] tracking-wider text-ink3 uppercase">Machines</h2>
      {machines.data?.map((machine) => {
        const ready = machine.agents.filter((found) => found.ready === "signed_in").map((found) => agents[found.agent].name)
        return (
          <Link key={machine.id} to="/settings/machines" className="flex items-center gap-2 rounded-md px-2 py-1.5 hover:bg-hov">
            <span className={`size-2 shrink-0 rounded-full ${machine.connected ? "bg-ok" : "bg-fld"}`} />
            <span className="flex min-w-0 flex-col">
              <span className="truncate">{machine.name}</span>
              <span className="truncate text-[12px] text-ink2">{machine.connected ? ready.join(", ") || "No agent signed in" : "Not connected"}</span>
            </span>
          </Link>
        )
      })}
      <Link to="/settings/machines" className="rounded-md px-2 py-1.5 text-ink2 hover:bg-hov">
        {machines.data?.length ? "Your machines" : "Add a machine"}
      </Link>
    </nav>
  )
}

function ServerStatus({ up }: { up: boolean }) {
  return (
    <span
      title={up ? "Server connected" : "Server unreachable"}
      className={`size-2 rounded-full ${up ? "bg-ok" : "bg-acc"}`}
    />
  )
}

function Account({ user, team, openTeam }: { user: User; team: Team; openTeam: (id: string) => void }) {
  const queryClient = useQueryClient()
  const signOut = useMutation({
    mutationFn: () => api("POST", "/sign-out"),
    // Everything remembered was the last account's.
    onSuccess: () => queryClient.resetQueries(),
  })
  return (
    <div className="flex flex-col gap-2 rounded-md border border-line p-2">
      <select
        aria-label="Team"
        className="rounded-md border border-fld bg-bg px-2 py-1.5 font-medium"
        value={team.id}
        onChange={(event) => openTeam(event.target.value)}
      >
        {user.teams.map((team) => (
          <option key={team.id} value={team.id}>
            {team.name}
          </option>
        ))}
      </select>
      <div className="truncate px-1 text-ink2">{user.email}</div>
      <div className="flex gap-2">
        <Link to="/settings/team" className="btn">
          Team settings
        </Link>
        <button className="btn" onClick={() => signOut.mutate()}>
          Sign out
        </button>
      </div>
    </div>
  )
}

function OwnerMail() {
  const config = useQuery({ queryKey: ["mail-config"], queryFn: () => api<MailConfig>("GET", "/config/mail") })
  if (!config.data) return null
  if (config.data.smtp) {
    return (
      <Link to="/settings/mail" className="btn self-start">
        Mail settings
      </Link>
    )
  }
  return (
    <div className="flex flex-col items-start gap-2 rounded-md border border-amb-line bg-amb-soft p-2 text-amb">
      Mail isn't set up. Sign-in codes are only printed to the server's log.
      <Link to="/settings/mail" className="btn">
        Set up mail
      </Link>
    </div>
  )
}
