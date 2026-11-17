import { useQuery } from "@tanstack/react-query"
import { get, type Health } from "./api"

const sections = ["Your chats", "Direct", "Groups", "Machines"]

export function Shell() {
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
      </aside>
      <main className="flex flex-1 items-center justify-center text-ink2">No chat open</main>
    </div>
  )
}

function ServerStatus() {
  const health = useQuery({
    queryKey: ["health"],
    queryFn: () => get<Health>("/health"),
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
