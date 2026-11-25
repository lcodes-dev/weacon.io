export type Health = { status: "ok" }

export type User = {
  id: string
  email: string
  name: string
  is_owner: boolean
  teams: Team[]
}

export type Team = { id: string; name: string; role: "owner" | "member" }

/** `key` is what the project's tasks are named by, as `SF` in SF-12. */
export type Project = { id: string; name: string; key: string; repo: string | null; branches: Branch[] }

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

export type Agent = "claude" | "codex"

/** What each agent is called, and its vendor's own installer for macOS, Linux and WSL. */
export const agents = {
  claude: { name: "Claude Code", installer: "curl -fsSL https://claude.ai/install.sh | bash" },
  codex: { name: "Codex", installer: "curl -fsSL https://chatgpt.com/codex/install.sh | sh" },
}

/** Your own computer or virtual machine, whose core runs agents for you. */
export type Machine = {
  id: string
  name: string
  os: "macos" | "linux"
  /** What its core last said of its agents. */
  agents: { agent: Agent; ready: "missing" | "signed_out" | "signed_in" }[]
  seen_at: number | null
  /** Whether its core is connected now. */
  connected: boolean
}

/** A person's conversation with an agent that works on a project, on one of that person's machines. */
export type Chat = {
  id: string
  title: string
  agent: Agent
  branch: string
  /** Whether the agent is working on a turn now. */
  running: boolean
  project_id: string
  project: string
  user_id: string
  user: string
  /** Null once the machine was removed. */
  machine: string | null
  /** The pull request the chat opened, once it has. */
  pull_number: number | null
  pull_url: string | null
  /** The hosts the chat's commands may reach. */
  hosts: string[]
  /** The task it is for, if any, and what that is called, as `SF-12 Returns link`. */
  task_id: string | null
  task: string | null
}

/** Some members of a team and what they write to each other. One without a name is called by its other members'. */
export type Group = {
  id: string
  name: string | null
  project_id: string | null
  project: string | null
  members: { id: string; name: string }[]
  /** Whether it has a message you haven't read. */
  unread: boolean
}

export type GroupMessage = { id: string; user_id: string | null; user: string | null; text: string; created_at: number }

export type OpenGroup = Group & { messages: GroupMessage[] }

/** What a group is called: its name, or the names of the others in it. */
export function groupName(group: Group, me: string) {
  const others = group.members.filter((member) => member.id !== me).map((member) => member.name)
  return group.name ?? (others.join(", ") || "Only you")
}

export const statuses = { todo: "To do", doing: "Doing", done: "Done" } as const

export type Status = keyof typeof statuses

/** Something a project needs done. It is named by its project's key and its number, as SF-12. */
export type Task = {
  id: string
  number: number
  title: string
  /** Markdown. */
  text: string
  /** What it is done when. */
  checks: { text: string; done: boolean }[]
  status: Status
  owner_id: string | null
  owner: string | null
  /** Who asked for it; null once that person's account is gone. */
  asker_id: string | null
  asker: string | null
  created_at: number
  /** The chat whose agent made it, if one did. */
  chat_id: string | null
  /** The pages of the project's docs it touches. */
  docs: string[]
}

/** What was written and done in a chat. A file's lines are counted against the commit the chat started from. */
export type ChatEvent = { id: string } & (
  | { kind: "asked"; text: string }
  | { kind: "said"; text: string }
  | { kind: "did"; deed: "read" | "edit" | "command" | "other"; what: string }
  | { kind: "done"; changed: { path: string; added: number; removed: number }[] }
  | { kind: "failed"; why: string }
  /** A command wanted a host the chat doesn't allow, and was refused. */
  | { kind: "wants"; host: string }
  | { kind: "allowed"; host: string }
  /** The agent made a task or changed one, which `name` calls as SF-12 Returns link. */
  | { kind: "task"; task_id: string; name: string; made: boolean }
)

/** A chat as its page shows it. Only the person it is `mine` for writes in it. */
export type OpenChat = Chat & { mine: boolean; connected: boolean; events: ChatEvent[] }

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
  /** The files the docs writer wrote it from, and those it was given and had no room to read. */
  files: string[]
  unread: string[]
  /** The commit its text was written from, which its citations point into. */
  commit: string | null
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
