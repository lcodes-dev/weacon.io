export type User = {
  id: string
  email: string
  name: string
  is_owner: boolean
  teams: Team[]
}

export type Team = { id: string; name: string; role: "owner" | "member" }

export type Project = { id: string; name: string; key: string; repo: string | null; branches: Branch[] }

export type Branch = { name: string; head_sha: string | null; commit_sha: string | null; error: string | null }

export type RepoBranches = { default: string; branches: string[] }

export type MapFile = {
  path: string
  language: string | null
  lines: number
  symbols: { name: string; kind: string; line: number; end_line: number }[]
}

export type CodeMap = { commit: string | null; files: MapFile[]; uses: [string, string][] }

export type Github = { app: string | null; account: string | null; repos: string[] }

export type Agent = "claude" | "codex"

export const agents = {
  claude: { name: "Claude Code", installer: "curl -fsSL https://claude.ai/install.sh | bash" },
  codex: { name: "Codex", installer: "curl -fsSL https://chatgpt.com/codex/install.sh | sh" },
}

export function who(person: string | null, agent: keyof typeof agents | "weacon" | null) {
  if (agent === "weacon") return "Weacon"
  return (person ?? "Someone who left") + (agent ? `'s ${agents[agent].name}` : "")
}

export type Machine = {
  id: string
  name: string
  os: "macos" | "linux"
  agents: { agent: Agent; ready: "missing" | "signed_out" | "signed_in" }[]
  seen_at: number | null
  connected: boolean
}

export type Chat = {
  id: string
  title: string
  agent: Agent
  branch: string
  running: boolean
  project_id: string
  project: string
  user_id: string
  user: string
  machine: string | null
  pull_number: number | null
  pull_url: string | null
  hosts: string[]
  task_id: string | null
  task: string | null
  group_id: string | null
}

export type Group = {
  id: string
  name: string | null
  project_id: string | null
  project: string | null
  members: { id: string; name: string }[]
  unread: boolean
}

export type GroupMessage = {
  id: string
  user_id: string | null
  user: string | null
  text: string
  created_at: number
  agent: Agent | "weacon" | null
  chat_id: string | null
  machine: string | null
}

export type OpenGroup = Group & { messages: GroupMessage[]; working: [string, Agent | "weacon"][] }

export function groupName(group: Group, me: string) {
  const others = group.members.filter((member) => member.id !== me).map((member) => member.name)
  return group.name ?? (others.join(", ") || "Only you")
}

export const statuses = { todo: "To do", doing: "Doing", done: "Done" } as const

export type Status = keyof typeof statuses

export type Task = {
  id: string
  number: number
  title: string
  text: string
  checks: { text: string; done: boolean }[]
  status: Status
  owner_id: string | null
  owner: string | null
  asker_id: string | null
  asker: string | null
  created_at: number
  chat_id: string | null
  docs: string[]
}

export type ChatEvent = { id: string } & (
  | { kind: "asked"; text: string }
  | { kind: "said"; text: string }
  | { kind: "did"; deed: "read" | "edit" | "command" | "other"; what: string }
  | { kind: "done"; changed: { path: string; added: number; removed: number }[] }
  | { kind: "failed"; why: string }
  | { kind: "wants"; host: string }
  | { kind: "allowed"; host: string }
  | { kind: "task"; task_id: string; name: string; made: boolean }
  | { kind: "proposal"; title: string; new: boolean }
)

export type OpenChat = Chat & { mine: boolean; connected: boolean; events: ChatEvent[] }

export type Writer = { kind: "claude_api" | "claude_code" | "codex"; model: string | null }

export type Docs = {
  state: "idle" | "running" | "failed"
  message: string | null
  branch: string | null
  commit: string | null
  left_out: string[]
  pages: DocsPage[]
  proposals: Proposal[]
}

type Section = "overview" | "works" | "built"

export type Note = { quote: string; says: string; place: string; instead: string | null }

export type DocsPage = {
  id: string
  section: Section
  title: string
  markdown: string
  version: string
  files: string[]
  unread: string[]
  commit: string | null
  notes: Note[] | null
  checking: boolean
}

export type Proposal = {
  id: string
  page: string | null
  base: string | null
  section: Section
  title: string
  markdown: string | null
  notes: Note[]
  reason: string
  request: string | null
  agent: keyof typeof agents | "weacon" | null
  user: string | null
  chat_id: string | null
}

export type DocVersion = { id: string; title: string; markdown: string; author: string | null; created_at: number }

export type Share = { token: string; created_by: string; created_at: number }

export type Shared = { title: string; created_at: number; pages: { title: string; markdown: string }[] }

export type TeamDetails = {
  name: string
  writer: Writer | null
  has_key: boolean
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
  password?: string
  from: string
}

export type MailConfig = { smtp: Smtp | null }

export async function api<T = void>(method: string, path: string, body?: unknown): Promise<T> {
  const file = body instanceof Blob
  const response = await fetch(`/api${path}`, {
    method,
    headers: body === undefined || file ? undefined : { "Content-Type": "application/json" },
    body: body === undefined || file ? body : JSON.stringify(body),
  })
  if (response.status === 202 || response.status === 204) return undefined as T
  const data = await response.json().catch(() => null)
  if (!response.ok || data === null) throw new Error(data?.error ?? `The server answered ${response.status}`)
  return data
}
