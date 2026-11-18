export type Health = { status: "ok" }

export type User = {
  id: number
  email: string
  name: string
  is_owner: boolean
  teams: Team[]
}

export type Team = { id: number; name: string; role: "owner" | "member" }

export type Project = { id: number; name: string; repo: string | null; branches: Branch[] }

/** A tracked branch. Its map is behind while `commit_sha` differs from `head_sha`. */
export type Branch = { name: string; head_sha: string | null; commit_sha: string | null; error: string | null }

export type RepoBranches = { default: string; branches: string[] }

export type MapFile = {
  path: string
  language: string | null
  lines: number
  symbols: { name: string; kind: string; line: number; end_line: number }[]
}

/** `uses` pairs a file with a file it uses. */
export type CodeMap = { commit: string | null; files: MapFile[]; uses: [string, string][] }

/** The team's GitHub App: its name, the account it is installed on, and the repositories it reaches. */
export type Github = { app: string | null; account: string | null; repos: string[] }

export type Writer = { kind: "claude_api" | "claude_code" | "codex"; model: string | null; has_key: boolean }

/** A project's generated docs, and the run that writes them. */
export type Docs = {
  state: "idle" | "running" | "failed"
  /** Where a run is, or why it failed. */
  message: string | null
  branch: string | null
  commit: string | null
  left_out: string[]
  pages: { id: number; section: "overview" | "works" | "built"; title: string; markdown: string }[]
}

export type TeamDetails = {
  name: string
  writer: Writer | null
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
