import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { OverlayHost } from "@ui-sdk/react/overlays"
import { lazy, Suspense, useState } from "react"
import { Link, Route, Routes } from "react-router"
import { api, type Health, type MailConfig, type Project, type Team, type User } from "../api"
import { MailSettings } from "../routes/MailSettings"
import { ProjectPage } from "../routes/Project"
import { TeamSettings } from "../routes/TeamSettings"

// The docs page is the largest, so it is fetched when first opened.
const DocsPage = lazy(() => import("../routes/ProjectDocs").then((module) => ({ default: module.DocsPage })))

const sections = ["Your chats", "Direct", "Groups", "Machines"]

export function Shell({ user }: { user: User }) {
  const [teamId, setTeamId] = useState(() => localStorage.getItem("team"))
  const team = user.teams.find((team) => team.id === teamId) ?? user.teams[0]
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
          <ServerStatus />
        </div>
        <button className="rounded-md bg-pri py-1.5 font-medium text-pri-text hover:bg-pri-hover">
          New chat
        </button>
        <nav className="flex flex-col">
          <a className="rounded-md px-2 py-1.5 hover:bg-hov">Tasks</a>
          <a className="rounded-md px-2 py-1.5 hover:bg-hov">Docs</a>
        </nav>
        <Projects team={team} />
        {sections.map((section) => (
          <h2 key={section} className="px-2 pt-2 font-mono text-[10px] tracking-wider text-ink3 uppercase">
            {section}
          </h2>
        ))}
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
          <Route path="/projects/:id" element={<ProjectPage team={team} />} />
          <Route path="/settings/team" element={<TeamSettings key={team.id} team={team} user={user} />} />
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

function ServerStatus() {
  const health = useQuery({
    queryKey: ["health"],
    queryFn: () => api<Health>("GET", "/health"),
    refetchInterval: 5000,
    retry: false,
  })
  const up = health.isSuccess
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
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["session"] }),
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
