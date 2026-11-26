import { useQueryClient } from "@tanstack/react-query"
import { useEffect, useState } from "react"

const asked: Record<string, string[]> = { chats: ["chat", "chats"], groups: ["group", "groups"] }

export function useLive() {
  const queryClient = useQueryClient()
  const [open, setOpen] = useState(false)
  useEffect(() => {
    let socket: WebSocket
    let again: number | undefined
    const connect = () => {
      socket = new WebSocket(`${location.origin.replace(/^http/, "ws")}/api/live`)
      socket.onopen = () => {
        setOpen(true)
        queryClient.invalidateQueries()
      }
      socket.onmessage = ({ data }) => (asked[data] ?? [data]).forEach((key) => queryClient.invalidateQueries({ queryKey: [key] }))
      socket.onclose = () => {
        setOpen(false)
        again = window.setTimeout(connect, 2000)
      }
    }
    connect()
    return () => {
      socket.onclose = null
      socket.close()
      clearTimeout(again)
    }
  }, [queryClient])
  return open
}
