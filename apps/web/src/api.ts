export type Health = { status: "ok" }

export type User = {
  id: number
  email: string
  name: string
  is_owner: boolean
  teams: Team[]
}

export type Team = { id: number; name: string; role: "owner" | "member" }

export type TeamDetails = {
  name: string
  members: { id: number; name: string; email: string; role: "owner" | "member" }[]
  invites: { id: number; email: string; expires_at: number }[]
}

export type Invitation = { team_name: string; invited_by: string; email: string; has_account: boolean }

export type Session = { setup_needed: boolean; user: User | null }

export type Smtp = {
  host: string
  port: number
  security: "tls" | "starttls" | "none"
  username: string | null
  /** Never read back; leave it out to keep the saved one. */
  password?: string
  from: string
}

export type MailConfig = { smtp: Smtp | null }

/** A failure throws an `Error` with the server's message. */
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
