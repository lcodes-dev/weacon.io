export type Health = { status: "ok" }

export type User = {
  id: number
  email: string
  name: string
  is_owner: boolean
  team_id: number
  team_name: string
}

export type Session = { setup_needed: boolean; user: User | null }

export type Smtp = {
  host: string
  port: number
  security: "tls" | "starttls" | "none"
  username: string | null
  /** Never read back. Leave it out when saving to keep the saved one. */
  password?: string
  from: string
}

export type MailConfig = { smtp: Smtp | null }

/** Calls the API. A failure throws an `Error` with the server's message. */
export async function api<T = void>(method: string, path: string, body?: unknown): Promise<T> {
  const response = await fetch(`/api${path}`, {
    method,
    headers: body === undefined ? undefined : { "Content-Type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  })
  if (response.status === 204) return undefined as T
  const data = await response.json()
  if (!response.ok) throw new Error(data.error)
  return data
}
