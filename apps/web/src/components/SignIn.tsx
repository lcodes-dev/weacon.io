import { FitAddon, init, Terminal } from "ghostty-web"
import { useEffect, useRef, useState } from "react"
import type { Agent, Machine } from "../api"

/** The terminal in which an agent's own sign-in runs on a machine. It shows and types, and keeps nothing. */
export function SignIn({ machine, agent, onDone }: { machine: Machine; agent: Agent; onDone: () => void }) {
  const place = useRef<HTMLDivElement>(null)
  // How the terminal ended: after showing something, or without ever opening.
  const [ended, setEnded] = useState<"done" | "refused">()
  useEffect(() => {
    let leave = () => {}
    let left = false
    init().then(() => {
      if (left) return
      const terminal = new Terminal({ fontSize: 13, theme: { background: "#141312", foreground: "#ece7df" } })
      const fit = new FitAddon()
      terminal.loadAddon(fit)
      terminal.open(place.current!)
      fit.fit()
      const scheme = location.protocol === "https:" ? "wss" : "ws"
      const socket = new WebSocket(`${scheme}://${location.host}/api/machines/${machine.id}/sign-in/${agent}`)
      const say = (message: object) => socket.readyState === WebSocket.OPEN && socket.send(JSON.stringify(message))
      let shown = false
      socket.onopen = () => say({ type: "size", cols: terminal.cols, rows: terminal.rows })
      socket.onmessage = (event) => {
        shown = true
        terminal.write(Uint8Array.from(atob(JSON.parse(event.data).data), (byte) => byte.charCodeAt(0)))
      }
      socket.onclose = () => setEnded(shown ? "done" : "refused")
      // What is typed or pasted goes as bytes, whatever its letters.
      terminal.onData((typed) => say({ type: "data", data: btoa(String.fromCharCode(...new TextEncoder().encode(typed))) }))
      terminal.onResize(({ cols, rows }) => say({ type: "size", cols, rows }))
      fit.observeResize()
      terminal.focus()
      leave = () => {
        socket.close()
        terminal.dispose()
      }
    })
    return () => {
      left = true
      leave()
    }
  }, [machine.id, agent])
  return (
    <div className="flex flex-col gap-2">
      <div ref={place} className="h-[360px] overflow-hidden rounded-lg border border-line bg-[#141312] p-2" />
      <div className="flex items-center gap-3">
        <button className="btn" onClick={onDone}>
          {ended ? "Close" : "Cancel"}
        </button>
        <span className="text-ink2">
          {ended === "refused" && "The sign-in couldn't be opened. The machine may have gone away, or the sign-in is open in another window."}
          {ended === "done" && "The sign-in has ended."}
          {!ended && "This is the vendor's own sign-in, running on your machine. Weacon passes it along and keeps none of it."}
        </span>
      </div>
    </div>
  )
}
