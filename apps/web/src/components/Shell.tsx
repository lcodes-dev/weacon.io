import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { OverlayHost } from "@ui-sdk/react/overlays"
import { CheckIcon, ChevronsUpDownIcon, LogOutIcon, MenuIcon, PlusIcon, SettingsIcon, TextIcon } from "lucide-react"
import { lazy, Suspense, useEffect, useState } from "react"
import { Link, NavLink, Route, Routes, useLocation, useMatch } from "react-router"
import { agents, api, groupName, type Chat, type Group, type Machine, type MailConfig, type Project, type Team, type TeamDetails, type User } from "../api"
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
  const [menuOpen, setMenuOpen] = useState(false)
  // On a phone the sidebar is a drawer, except with no page open, where it is the page.
  const home = useLocation().pathname === "/"
  const sidebarOnPhone = home
    ? "phone:w-full phone:border-0"
    : "phone:fixed phone:inset-y-0 phone:left-0 phone:z-30 phone:w-[min(304px,86vw)] phone:-translate-x-full phone:transition-transform phone:data-open:translate-x-0"
  const openTeam = (id: string) => {
    localStorage.setItem("team", id)
    setTeamId(id)
  }
  useEffect(() => {
    if (!menuOpen) return
    const closeOnEscape = (event: KeyboardEvent) => event.key === "Escape" && setMenuOpen(false)
    addEventListener("keydown", closeOnEscape)
    return () => removeEventListener("keydown", closeOnEscape)
  }, [menuOpen])
  return (
    <div className="flex h-dvh">
      {menuOpen && <div className="fixed inset-0 z-20 bg-black/50 not-phone:hidden" onClick={() => setMenuOpen(false)} />}
      <aside
        id="sidebar"
        aria-label="Sidebar"
        data-open={menuOpen || undefined}
        onClick={(event) => (event.target as Element).closest("a") && setMenuOpen(false)}
        className={`flex w-[272px] shrink-0 flex-col border-r border-line bg-side phone:overflow-y-auto ${sidebarOnPhone}`}
      >
        <div className="flex items-center gap-2.5 px-4 pt-3 pb-3 phone:pt-[max(12px,env(safe-area-inset-top))]">
          <Logo />
          <span className="flex-1" />
          <ServerStatus up={live} />
        </div>
        <div className="px-2.5">
          <Link to="/chats/new" className="btn btn-primary w-full justify-center">
            New chat
          </Link>
        </div>
        <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-2.5 pt-2.5 pb-3 phone:flex-none phone:overflow-visible">
          <Modules team={team} />
          <Projects team={team} />
          <Groups team={team} user={user} />
          <Chats team={team} user={user} />
          <YourMachines />
        </div>
        <div className="flex flex-col gap-2 border-t border-line p-2.5 phone:pb-[max(10px,env(safe-area-inset-bottom))]">
          {user.is_owner && <OwnerMail />}
          <TeamCard user={user} team={team} openTeam={openTeam} />
          <Account user={user} />
        </div>
      </aside>
      <main className={`flex min-w-0 flex-1 flex-col ${home ? "phone:hidden" : ""}`}>
        <div className="flex h-12 shrink-0 items-center gap-1 border-b border-line bg-side px-1 pt-[env(safe-area-inset-top)] not-phone:hidden">
          <button type="button" className="icb" aria-label="Menu" aria-controls="sidebar" aria-expanded={menuOpen} onClick={() => setMenuOpen(true)}>
            <MenuIcon />
          </button>
          <Link to="/" className="flex h-11 items-center">
            <Logo />
          </Link>
        </div>
        <div className="min-h-0 flex-1 overflow-auto">
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
        </div>
      </main>
      <OverlayHost />
    </div>
  )
}

function Logo() {
  return (
    <span className="flex items-center gap-2.5">
      <img src="/logo.svg" alt="" className="-m-1.5 size-[34px]" />
      <span className="text-[16px] font-bold tracking-[-0.02em]">weacon</span>
    </span>
  )
}

function useProjects(team: Team) {
  return useQuery({
    queryKey: ["projects", team.id],
    queryFn: () => api<Project[]>("GET", `/teams/${team.id}/projects`),
  })
}

/** Tasks and Docs, of the project that is open or else the first. */
function Modules({ team }: { team: Team }) {
  const projects = useProjects(team)
  const openId = useMatch("/projects/:id/*")?.params.id
  const project = projects.data?.find((project) => project.id === openId) ?? projects.data?.[0]
  if (!project) return null
  return (
    <nav className="flex flex-col">
      <NavLink to={`/projects/${project.id}/tasks`} className="nav">
        <span className="flex size-[18px] items-center justify-center rounded-[5px] bg-acc-fill text-white">
          <CheckIcon className="size-3" strokeWidth={3} />
        </span>
        Tasks
      </NavLink>
      <NavLink to={`/projects/${project.id}/docs`} className="nav">
        <span className="flex size-[18px] items-center justify-center rounded-[5px] bg-vio-fill text-white">
          <TextIcon className="size-3" strokeWidth={3} />
        </span>
        Docs
      </NavLink>
    </nav>
  )
}

function Projects({ team }: { team: Team }) {
  const projects = useProjects(team)
  return (
    <nav className="flex flex-col">
      <h2 className="sec">Projects</h2>
      {projects.data?.map((project) => (
        <NavLink key={project.id} to={`/projects/${project.id}`} end className="nav">
          <span className="w-6 shrink-0 font-mono text-[10px] text-ink3">{project.key}</span>
          <span className="truncate">{project.name}</span>
        </NavLink>
      ))}
      <NavLink to="/projects/new" className="nav text-ink2">
        <PlusIcon className="size-3.5 w-6" />
        New project
      </NavLink>
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
      <h2 className="sec">Groups</h2>
      {groups.data?.map((group) => (
        <NavLink key={group.id} to={`/groups/${group.id}`} className="nav">
          <span className="w-6 shrink-0 text-center text-ink3">#</span>
          <span className="flex min-w-0 flex-1 flex-col">
            <span className={`truncate ${group.unread ? "font-semibold" : ""}`}>{groupName(group, user.id)}</span>
            <span className="sub">{group.project ?? `${group.members.length} people`}</span>
          </span>
          {group.unread && <span className="size-2 shrink-0 rounded-full bg-acc-fill" aria-label="Unread" />}
        </NavLink>
      ))}
      <NavLink to="/groups/new" className="nav text-ink2">
        <PlusIcon className="size-3.5 w-6" />
        New group
      </NavLink>
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
      <h2 className="sec">{title}</h2>
      {chats.length === 0 && <p className="px-2 py-1 text-[12px] text-ink3">None yet</p>}
      {chats.map((chat) => (
        <NavLink key={chat.id} to={`/chats/${chat.id}`} className="nav">
          <Dot className={chat.running ? "animate-pulse bg-ok" : "bg-fld"} />
          <span className="flex min-w-0 flex-col">
            <span className="truncate">{chat.title}</span>
            <span className="sub">
              {whose && `${chat.user} · `}
              {chat.project} · {agents[chat.agent].name}
            </span>
          </span>
        </NavLink>
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
      <h2 className="sec">Machines</h2>
      {machines.data?.map((machine) => {
        const ready = machine.agents.filter((found) => found.ready === "signed_in").map((found) => agents[found.agent].name)
        return (
          <Link key={machine.id} to="/settings/machines" className="nav">
            <Dot className={machine.connected ? "bg-ok" : "bg-fld"} />
            <span className="flex min-w-0 flex-col">
              <span className="truncate">{machine.name}</span>
              <span className="sub">{machine.connected ? ready.join(", ") || "No agent signed in" : "Not connected"}</span>
            </span>
          </Link>
        )
      })}
      <NavLink to="/settings/machines" end className="nav text-ink2">
        <PlusIcon className="size-3.5 w-6" />
        {machines.data?.length ? "Your machines" : "Add a machine"}
      </NavLink>
    </nav>
  )
}

/** A status dot, in the 24px column where other rows have an icon. */
function Dot({ className }: { className: string }) {
  return (
    <span className="flex w-6 shrink-0 justify-center">
      <span className={`size-[7px] rounded-full ${className}`} />
    </span>
  )
}

function ServerStatus({ up }: { up: boolean }) {
  return <span title={up ? "Server connected" : "Server unreachable"} className={`size-[7px] rounded-full ${up ? "bg-ok" : "bg-acc"}`} />
}

/** The first letters of the first two words, as in "NW" for Northwind or "AD" for Ada Dorn. */
function initials(name: string) {
  const words = name.trim().split(/\s+/)
  return (words.length > 1 ? words[0][0] + words[1][0] : name.slice(0, 2)).toUpperCase()
}

/** The open team, and the switch to another while you are in several. */
function TeamCard({ user, team, openTeam }: { user: User; team: Team; openTeam: (id: string) => void }) {
  const details = useQuery({ queryKey: ["team", team.id], queryFn: () => api<TeamDetails>("GET", `/teams/${team.id}`) })
  const people = details.data?.members.length
  return (
    <div className="relative flex items-center gap-2.5 rounded-[10px] border border-line bg-sur px-2 py-1.5 has-[select:hover]:bg-hov">
      <span className="initials bg-team">{initials(team.name)}</span>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate font-semibold">{team.name}</span>
        <span className="sub">Team{people && ` · ${people} ${people === 1 ? "person" : "people"}`}</span>
      </span>
      {user.teams.length > 1 && (
        <>
          <ChevronsUpDownIcon className="size-3.5 text-ink3" />
          <select aria-label="Team" className="absolute inset-0 cursor-pointer opacity-0" value={team.id} onChange={(event) => openTeam(event.target.value)}>
            {user.teams.map((team) => (
              <option key={team.id} value={team.id}>
                {team.name}
              </option>
            ))}
          </select>
        </>
      )}
    </div>
  )
}

function Account({ user }: { user: User }) {
  const queryClient = useQueryClient()
  const signOut = useMutation({
    mutationFn: () => api("POST", "/sign-out"),
    // Everything remembered was the last account's.
    onSuccess: () => queryClient.resetQueries(),
  })
  return (
    <div className="flex items-center gap-2.5 rounded-[10px] border border-line bg-sur px-2 py-1.5">
      <span className="initials rounded-full bg-acc-fill">{initials(user.name || user.email)}</span>
      <span className="flex min-w-0 flex-1 flex-col">
        <span className="truncate font-semibold">{user.name}</span>
        <span className="sub">{user.email}</span>
      </span>
      <Link to="/settings/team" className="icb" aria-label="Team settings" title="Team settings">
        <SettingsIcon />
      </Link>
      <button type="button" className="icb" aria-label="Sign out" title="Sign out" onClick={() => signOut.mutate()}>
        <LogOutIcon />
      </button>
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
