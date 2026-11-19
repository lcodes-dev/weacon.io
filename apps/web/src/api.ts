export type Health = { status: "ok" }

export type User = {
  id: string
  email: string
  name: string
  is_owner: boolean
  teams: Team[]
}

export type Team = { id: string; name: string; role: "owner" | "member" }

export type Project = { id: string; name: string; repo: string | null; branches: Branch[] }

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
  pages: DocsPage[]
  proposals: Proposal[]
}

type Section = "overview" | "works" | "built"

/** Where the docs writer finds a person's sentence at odds with the code. `instead` is the sentence corrected, when it offers one. */
export type Note = { quote: string; says: string; place: string; instead: string | null }

export type DocsPage = {
  id: string
  section: Section
  title: string
  markdown: string
  /** The id of the page's newest version, which an edit names as what it started from. */
  version: string
  /** The writer's notes on the page as it is, or null when it hasn't checked it. */
  notes: Note[] | null
  /** Whether the writer is checking the page after an edit. */
  checking: boolean
}

/** A change the docs writer suggests, which changes nothing until it is accepted. */
export type Proposal = {
  id: string
  /** The page to change or remove, or null for a new page. */
  page: string | null
  /** The page's version it was written against. */
  base: string | null
  section: Section
  title: string
  /** Null to remove the page. */
  markdown: string | null
  notes: Note[]
  reason: string
  /** What a person asked for, when the proposal answers a request. */
  request: string | null
}

/** One of the texts a page has had. `author` is null for the docs writer. */
export type DocVersion = { id: string; title: string; markdown: string; author: string | null; created_at: number }

/** A public link to a copy of a project's user docs. */
export type Share = { token: string; created_by: string; created_at: number }

/** What a public link shows. */
export type Shared = { title: string; created_at: number; pages: { title: string; markdown: string }[] }

export type TeamDetails = {
  name: string
  writer: Writer | null
  members: { id: string; name: string; email: string; role: "owner" | "member" }[]
  invites: { id: string; email: string; expires_at: number }[]
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

/** A failure throws an `Error` with the server's message. A `Blob` is sent as it is, anything else as JSON. */
export async function api<T = void>(method: string, path: string, body?: unknown): Promise<T> {
  const file = body instanceof Blob
  const response = await fetch(`/api${path}`, {
    method,
    headers: body === undefined || file ? undefined : { "Content-Type": "application/json" },
    body: body === undefined || file ? body : JSON.stringify(body),
  })
  // 202 says that work has started, and like 204 comes with nothing to read.
  if (response.status === 202 || response.status === 204) return undefined as T
  // An error from something in front of the server, such as a proxy, may not be JSON.
  const data = await response.json().catch(() => null)
  if (!response.ok || data === null) throw new Error(data?.error ?? `The server answered ${response.status}`)
  return data
}
