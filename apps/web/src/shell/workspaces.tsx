import { type ReactNode, type RefObject, useEffect, useRef, useState } from "react";

import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { type Loading, answered, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { workspaceOf } from "../chat-ref.ts";
import { useChatAccess } from "../chat/access.ts";
import { ChatScreen } from "../chat/chat-screen.tsx";
import { type Status, statusText } from "../connection.ts";
import type { Log } from "../log.ts";
import type { Names } from "../members/names.ts";
import { DocsPage } from "../docs/docs-page.tsx";
import { useNames } from "../members/use-names.ts";
import type { WorkspaceId, WorkspaceList, WorkspaceSummary } from "../protocol.ts";
import { ProposalsPage } from "../proposals/proposals-page.tsx";
import { TasksPage } from "../tasks/tasks-page.tsx";
import { owns } from "./chat-list.ts";
import { ChatsNav } from "./chats.tsx";
import { useFeed, useReloadOn } from "./feed-hooks.ts";
import { Frame } from "./frame.tsx";
import { STORE, opening, remember } from "./last-workspace.ts";
import { type Place, placeOf, searchOf } from "./place.ts";
import { type WorkspaceSocket, useOffers, useSocketFor } from "./workspace-socket.ts";

/**
 * The signed-in person's workspaces, and the one `?workspace=` names, or else the last one opened
 * here, open on its feed with a selector in the header; `who` is the header's account part. A
 * `page`, such as Settings, shows in their place while they stay open beneath it, given the open
 * workspace's socket.
 */
export function Workspaces({
  api,
  log,
  who,
  page,
}: {
  api: Api;
  log: Log;
  who: ReactNode;
  page?: ((socket: WorkspaceSocket | undefined) => ReactNode) | undefined;
}) {
  const loaded = useLoaded(() => api.workspaces(), api);
  const list = loaded.value?.workspaces;
  const { open, place, choose, go } = useOpenWorkspace();
  const workspace = list?.find(({ id }) => id === open);
  const socket = useSocketFor(workspace?.id);
  // Whether the list is on screen when a workspace made from it arrives, so one made after the
  // person moved on doesn't pull them back, and one made before they came back does open.
  const listing = useRef(false);
  const shown = workspace?.id;
  const covered = page !== undefined;
  useEffect(() => {
    listing.current = shown === undefined && !covered;
    return () => {
      listing.current = false;
    };
  }, [shown, covered]);
  const selector = list !== undefined && workspace !== undefined && (
    <span className="selector">
      <select aria-label="Workspace" value={workspace.id} onChange={(event) => choose(event.target.value)}>
        {list.map(({ id, name }) => (
          <option key={id} value={id}>
            {name}
          </option>
        ))}
      </select>
      <button type="button" onClick={() => choose(undefined)}>
        All workspaces
      </button>
    </span>
  );
  return (
    <Frame middle={page === undefined && selector} right={who}>
      <div className={page === undefined ? "pane" : "pane away"}>
        {workspace !== undefined && socket !== undefined ? (
          <Workspace key={workspace.id} api={api} log={log} workspace={workspace} socket={socket} away={covered} place={place} go={go} />
        ) : (
          <AllWorkspaces api={api} loaded={loaded} listing={listing} choose={choose} />
        )}
      </div>
      {page !== undefined && <div className="pane">{page(socket)}</div>}
    </Frame>
  );
}

// The workspace and the place in it the address names, kept in step with Back and Forward and remembered here.
function useOpenWorkspace() {
  const [open, setOpen] = useState(() => opening(window.location.search, STORE));
  const [place, setPlace] = useState(() => placeIn(window.location.search));
  useEffect(() => {
    const follow = () => {
      setOpen(workspaceOf(window.location.search));
      setPlace(placeIn(window.location.search));
    };
    window.addEventListener("popstate", follow);
    return () => window.removeEventListener("popstate", follow);
  }, []);
  useEffect(() => {
    remember(STORE, open);
    // One opened from memory takes its place in the address, so Back and links find it.
    if (open !== undefined && workspaceOf(window.location.search) !== open) {
      window.history.replaceState(null, "", `?workspace=${open}`);
    }
  }, [open]);
  const choose = (id: WorkspaceId | undefined) => {
    window.history.pushState(null, "", id === undefined ? window.location.pathname : `?workspace=${id}`);
    setOpen(id);
    setPlace({ view: "chats" });
  };
  const go = (next: Place) => {
    const search = searchOf(open, next);
    if (search !== window.location.search) {
      window.history.pushState(null, "", search);
      setPlace(next);
    }
  };
  return { open, place, choose, go };
}

// A place only counts in an address that names its workspace.
function placeIn(search: string): Place {
  return workspaceOf(search) === undefined ? { view: "chats" } : placeOf(search);
}

// Every workspace, to open one, and a form to make another.
function AllWorkspaces({
  api,
  loaded,
  listing,
  choose,
}: {
  api: Api;
  loaded: Loading<WorkspaceList>;
  listing: RefObject<boolean>;
  choose(id: WorkspaceId): void;
}) {
  const list = loaded.value?.workspaces;
  return (
    <section className="page workspaces">
      <h1>Workspaces</h1>
      {loaded.loading && <p className="muted">Loading</p>}
      {list !== undefined && (
        <ul>
          {list.map(({ id, name, role }) => (
            <li key={id}>
              <button type="button" onClick={() => choose(id)}>
                {name}
              </button>
              {role === "owner" && <span className="muted">Owner</span>}
            </li>
          ))}
        </ul>
      )}
      {loaded.problem !== undefined && <Problem text={loaded.problem} retry={() => void loaded.load()} />}
      {list !== undefined && (
        <NewWorkspace
          api={api}
          made={(made) => {
            loaded.change((before) => ({ workspaces: [...(before?.workspaces ?? []), made] }));
            if (listing.current) {
              choose(made.id);
            }
          }}
        />
      )}
    </section>
  );
}

function NewWorkspace({ api, made }: { api: Api; made(workspace: WorkspaceSummary): void }) {
  const [name, setName] = useState("");
  const { busy, problem, ask } = useAsk();
  return (
    <form
      onSubmit={(event) => {
        event.preventDefault();
        void ask(
          () => api.createWorkspace(name),
          (workspace) => {
            setName("");
            made(workspace);
          },
        );
      }}
    >
      <h2>New workspace</h2>
      <label>
        Name
        <input required value={name} onChange={(event) => setName(event.target.value)} />
      </label>
      <button type="submit" disabled={busy}>
        Create
      </button>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </form>
  );
}

type Open = { api: Api; workspace: WorkspaceSummary; socket: WorkspaceSocket; away: boolean; place: Place; go(place: Place): void };

/** The open workspace on its feed: its sections, and its chats or the section `place` names. */
function Workspace({ log, ...open }: Open & { log: Log }) {
  const { api, workspace, socket, place } = open;
  const status = useFeed(workspace.id, api, socket, log);
  // Whether the server serves chats, known once the feed is welcomed.
  const { read } = useChatAccess(socket);
  const names = useNames(api, workspace.id, socket);
  if (place.view === "chats") {
    return <WorkspaceChats {...open} place={place} status={status} read={read} names={names} />;
  }
  return (
    <div className="workspace">
      <div className="side">
        <Sections {...open} />
      </div>
      <section>
        <Heading title={TITLES[place.view]} status={status} />
        {place.view === "tasks" ? (
          <TasksPage api={api} workspace={workspace.id} socket={socket} />
        ) : place.view === "docs" ? (
          <DocsPage api={api} workspace={workspace.id} socket={socket} names={names} doc={place.doc} choose={(doc) => open.go(doc === undefined ? { view: "docs" } : { view: "docs", doc })} />
        ) : (
          <ProposalsPage api={api} workspace={workspace.id} socket={socket} names={names} />
        )}
      </section>
    </div>
  );
}

const TITLES = { chats: "Chats", tasks: "Tasks", docs: "Docs", proposals: "Proposals" } as const;

// The workspace's sections the server serves, besides its chats; none when it serves no other.
function Sections({ socket, place, go }: Pick<Open, "socket" | "place" | "go">) {
  const served = {
    tasks: useOffers(socket, "workspace.tasks"),
    docs: useOffers(socket, "workspace.docs"),
    proposals: useOffers(socket, "workspace.proposals"),
  };
  const views = (["chats", "tasks", "docs", "proposals"] as const).filter((view) => view === "chats" || served[view]);
  if (views.length === 1) {
    return null;
  }
  return (
    <nav className="sections" aria-label="Sections">
      {views.map((view) => (
        <button key={view} type="button" aria-current={view === place.view ? "page" : undefined} onClick={() => go({ view })}>
          {TITLES[view]}
        </button>
      ))}
    </nav>
  );
}

function Heading({ title, status }: { title: string; status: Status }) {
  return (
    <div className="heading">
      <h1>{title}</h1>
      <p className="status">{statusText(status)}</p>
    </div>
  );
}

/**
 * The workspace's chats, where the server serves them (`read`), kept fresh by its feed, beside the
 * chosen one. Only its owner sends to a chat; a person whose member the server didn't say owns none.
 */
function WorkspaceChats({
  api,
  workspace,
  socket,
  status,
  away,
  place,
  go,
  read,
  names,
}: Open & { place: Extract<Place, { view: "chats" }>; status: Status; read: boolean; names: Names }) {
  const chats = useLoaded(() => (read ? api.chats(workspace.id) : answered({ chats: [] })), `${workspace.id} ${read}`);
  useReloadOn(socket, ({ kind }) => read && kind === "chat_changed", () => void chats.load(), [read]);
  const open = chats.value?.chats.find(({ id }) => id === place.chat);
  const listed = read ? chats : status.state === "connecting" ? "connecting" : "none";
  return (
    <div className="workspace">
      <div className="side">
        <Sections socket={socket} place={place} go={go} />
        <ChatsNav loaded={listed} chosen={place.chat} choose={(chat) => go({ view: "chats", chat })} />
      </div>
      <section>
        <Heading title={open?.title ?? workspace.name} status={status} />
        {open !== undefined && (
          <ChatScreen
            key={open.id}
            link={socket}
            live={status.state === "live"}
            chatRef={{ workspace: workspace.id, chat: open.id }}
            mine={owns(workspace.member, open)}
            away={away}
            title={open.title}
            names={names}
          />
        )}
      </section>
    </div>
  );
}
