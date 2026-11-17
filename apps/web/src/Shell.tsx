import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query"
import { api, type Health, type MailConfig, type User } from "./api"
import { MailSettings } from "./MailSettings"

const sections = ["Your chats", "Direct", "Groups", "Machines"]

export function Shell({ user }: { user: User }) {
  return (
    <div className="flex h-screen">
      <aside className="flex w-[272px] shrink-0 flex-col gap-3 border-r border-line bg-side p-3">
        <div className="flex items-center justify-between px-1">
          <span className="text-[15px] font-semibold">weacon</span>
          <ServerStatus />
        </div>
        <button className="rounded-md bg-pri py-1.5 font-medium text-pri-text hover:bg-pri-hover">
          New chat
        </button>
        <nav className="flex flex-col">
          <a className="rounded-md px-2 py-1.5 hover:bg-hov">Tasks</a>
          <a className="rounded-md px-2 py-1.5 hover:bg-hov">Docs</a>
        </nav>
        {sections.map((section) => (
          <h2 key={section} className="px-2 pt-2 font-mono text-[10px] tracking-wider text-ink3 uppercase">
            {section}
          </h2>
        ))}
        <div className="mt-auto flex flex-col gap-2">
          {user.is_owner && <OwnerMail />}
          <Account user={user} />
        </div>
      </aside>
      <main className="flex flex-1 items-center justify-center text-ink2">No chat open</main>
    </div>
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

function Account({ user }: { user: User }) {
  const queryClient = useQueryClient()
  const signOut = useMutation({
    mutationFn: () => api("POST", "/sign-out"),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["session"] }),
  })
  return (
    <div className="flex items-center gap-2 rounded-md border border-line p-2">
      <div className="min-w-0 flex-1">
        <div className="truncate font-medium">{user.team_name}</div>
        <div className="truncate text-ink2">{user.email}</div>
      </div>
      <button className="btn" onClick={() => signOut.mutate()}>
        Sign out
      </button>
    </div>
  )
}

function OwnerMail() {
  const config = useQuery({ queryKey: ["mail-config"], queryFn: () => api<MailConfig>("GET", "/config/mail") })
  if (!config.data) return null
  const { smtp } = config.data
  // One element for both states, so saving doesn't close the dialog.
  return (
    <div className={smtp ? "flex flex-col" : "flex flex-col items-start gap-2 rounded-md border border-amb-line bg-amb-soft p-2 text-amb"}>
      {!smtp && "Mail isn't set up. Sign-in codes are only printed to the server's log."}
      <MailSettings saved={smtp} trigger={smtp ? "Mail settings" : "Set up mail"} />
    </div>
  )
}
