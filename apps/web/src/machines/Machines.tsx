import { useMutation, useQueryClient } from "@tanstack/react-query"
import { confirm } from "@ui-sdk/react/alert-dialog"
import { lazy, Suspense, useState } from "react"
import { type Agent, agents, api } from "../api"
import { useMachines } from "../queries"

const SignIn = lazy(() => import("./AgentSignIn").then((module) => ({ default: module.SignIn })))

export function Machines() {
  const queryClient = useQueryClient()
  const [signing, setSigning] = useState<{ machine: string; agent: Agent }>()
  const machines = useMachines()
  const refresh = () => queryClient.invalidateQueries({ queryKey: ["machines"] })
  const code = useMutation({ mutationFn: () => api<{ code: string; server: string }>("POST", "/machines/codes") })
  const remove = useMutation({ mutationFn: (id: string) => api("DELETE", `/machines/${id}`), onSuccess: refresh })
  const command = code.data && `weacon-core pair ${code.data.server} ${code.data.code}`
  const error = code.error ?? remove.error
  return (
    <div className="page">
      <h1>Machines</h1>
      <p className="text-ink2">
        A machine is your own computer or virtual machine. Agents run there on your own sign-in, for every team you are in, and nobody else sees or
        uses it.
      </p>
      {error && <p className="text-acc">{error.message}</p>}
      {machines.data?.map((machine) => (
        <section key={machine.id} className="flex flex-col gap-2 rounded-lg border border-line p-3">
          <div className="flex items-center gap-2">
            <span className={`size-2 rounded-full ${machine.connected ? "bg-ok" : "bg-fld"}`} />
            <h2 className="mr-auto !pt-0">{machine.name}</h2>
            <button
              className="btn btn-sm"
              disabled={remove.isPending}
              onClick={async () => {
                const sure = await confirm({
                  title: `Remove "${machine.name}"?`,
                  description: "Its core is cut off and forgets this Weacon. The agents on it stay installed and signed in.",
                  confirmLabel: "Remove",
                })
                if (sure) remove.mutate(machine.id)
              }}
            >
              Remove
            </button>
          </div>
          <span className="lbl">
            {machine.os === "macos" ? "macOS" : "Linux"} ·{" "}
            {machine.connected
              ? "connected"
              : machine.seen_at
                ? `last seen ${new Date(machine.seen_at * 1000).toLocaleString()}`
                : "never connected: run weacon-core there"}
          </span>
          {machine.agents.map(({ agent, ready }) => (
            <div key={agent} className="flex flex-col gap-2">
              <div className="flex items-center gap-2">
                <span className="w-28 font-medium">{agents[agent].name}</span>
                {ready === "signed_in" && <span className="text-ok">Signed in</span>}
                {ready === "signed_out" && <span className="mr-auto text-ink2">Not signed in</span>}
                {ready === "signed_out" && machine.connected && !signing && (
                  <button className="btn btn-sm" onClick={() => setSigning({ machine: machine.id, agent })}>
                    Sign in
                  </button>
                )}
                {ready === "missing" && (
                  <span className="text-ink2">
                    Not installed. On the machine: <code className="font-mono text-[12px]">{agents[agent].installer}</code>
                  </span>
                )}
              </div>
              {signing?.machine === machine.id && signing.agent === agent && (
                <Suspense>
                  <SignIn
                    machine={machine}
                    agent={agent}
                    onDone={() => {
                      setSigning(undefined)
                      refresh()
                    }}
                  />
                </Suspense>
              )}
            </div>
          ))}
        </section>
      ))}
      <h2>Add a machine</h2>
      <p className="text-ink2">
        Install <code className="font-mono text-[12px]">weacon-core</code> on the machine, then run there the command this gives you. It works once, for
        ten minutes.
      </p>
      {command ? (
        <div className="flex items-center gap-2">
          <code className="min-w-0 flex-1 overflow-x-auto rounded-lg border border-line bg-sur px-3 py-2 font-mono text-[12px] whitespace-nowrap">
            {command}
          </code>
          <button className="btn" onClick={() => navigator.clipboard.writeText(command)}>
            Copy
          </button>
          <button className="btn" disabled={code.isPending} onClick={() => code.mutate()}>
            New code
          </button>
        </div>
      ) : (
        <button className="btn btn-primary self-start" disabled={code.isPending} onClick={() => code.mutate()}>
          Add a machine
        </button>
      )}
      <p className="text-ink2">
        For a server of yours that you reach over SSH, run <code className="font-mono text-[12px]">weacon-core add user@host</code> on a machine that
        is already paired.
      </p>
    </div>
  )
}
