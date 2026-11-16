import { type ReactNode, useEffect, useRef, useState } from "react";

import type { Api } from "../api/api.ts";
import { answered, useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { workspaceOf } from "../chat-ref.ts";
import { useChatAccess } from "../chat/access.ts";
import { ChatScreen } from "../chat/chat-screen.tsx";
import { type Status, statusText } from "../connection.ts";
import { Palette } from "../palette/palette.tsx";
import type { Names } from "../members/names.ts";
import { DocsPage } from "../docs/docs-page.tsx";
import { useMembers, useNames } from "../members/use-names.ts";
import type { ChatId, ChatLeg, MachineId, WorkspaceId, WorkspaceSummary } from "../protocol.ts";
import { NewChat, StartMissing } from "../new-chat/new-chat.tsx";
import { onceSharedKnown, readiness } from "../new-chat/start.ts";
import { ProposalsPage } from "../proposals/proposals-page.tsx";
import { TasksPage } from "../tasks/tasks-page.tsx";
import { useTasks } from "../tasks/use-tasks.ts";
import { ChatTaskLink } from "./chat-task-link.tsx";
import { firstLeg, reached, withFirst } from "./chat-list.ts";
import { ChatsNav } from "./chats.tsx";
import { useFeed, useReloadOn } from "./feed-hooks.ts";
import { Departures } from "./departures.ts";
import { Frame } from "./frame.tsx";
import { Landing } from "./landing.tsx";
import { STORE, opening, remember } from "./last-workspace.ts";
import { type Place, placeOf, searchOf } from "./place.ts";
import { WorkspaceMenu } from "./workspace-menu.tsx";
import { type WorkspaceSocket, useOffers, useSocketFor } from "./workspace-socket.ts";

/** The open workspace, as a page shown over it, such as Settings, sees it. */
export type OpenWorkspace = { workspace: WorkspaceSummary; socket: WorkspaceSocket; left(notice: string): void };

/**
 * The signed-in person's workspaces, and the one `?workspace=` names, or else the last one opened
 * here, open on its feed with a selector at the foot of its sidebar; `who` is the header's account
 * part. A `page`, such as Settings, shows in their place while they stay open beneath it. When the
 * server stops letting the person into `left`'s workspace, it closes, and the landing says why.
 */
export function Workspaces({
  api,
  who,
  email,
  left,
  page,
  openSettings,
}: {
  api: Api;
  who: ReactNode;
  email: string;
  left: { workspace: WorkspaceId } | undefined;
  page?: ((open: OpenWorkspace | undefined) => ReactNode) | undefined;
  /** Opens Settings, where the person pairs machines and signs agents in. */
  openSettings(machine?: MachineId): void;
}) {
  const loaded = useLoaded(() => api.workspaces(), api);
  const list = loaded.value?.workspaces;
  const { open, place, choose, go } = useOpenWorkspace();
  const workspace = list?.find(({ id }) => id === open);
  const socket = useSocketFor(workspace?.id);
  const [notice, setNotice] = useState<string>();
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
  // A workspace left goes from the list, which then shows in its place, saying why.
  const leave = (text: string) => {
    setNotice(text);
    void loaded.load();
  };
  const [departures] = useState(() => new Departures());
  useEffect(() => departures.listed(list?.map(({ id }) => id) ?? []), [list]);
  useEffect(() => {
    if (left !== undefined && departures.leave(left.workspace)) {
      const name = list?.find(({ id }) => id === left.workspace)?.name;
      leave(name === undefined ? "You're no longer a member of that workspace." : `You're no longer a member of ${name}.`);
    }
  }, [left]);
  const pick = (id: WorkspaceId | undefined) => {
    setNotice(undefined);
    choose(id);
  };
  const menu = workspace !== undefined && <WorkspaceMenu api={api} loaded={loaded} workspace={workspace} email={email} choose={pick} />;
  const opened = workspace !== undefined && socket !== undefined ? { workspace, socket, left: (text: string) => {
          departures.leave(workspace.id);
          leave(text);
        },
      } : undefined;
  return (
    <Frame right={who}>
      <div className={page === undefined ? "pane" : "pane away"}>
        {workspace !== undefined && socket !== undefined ? (
          <Workspace key={workspace.id} api={api} workspace={workspace} socket={socket} away={covered} place={place} go={go} openSettings={openSettings} menu={menu} />
        ) : (
          <Landing
            api={api}
            loaded={loaded}
            listing={listing}
            notice={notice}
            choose={pick}
          />
        )}
      </div>
      {page !== undefined && <div className="pane">{page(opened)}</div>}
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

type Open = {
  api: Api;
  workspace: WorkspaceSummary;
  socket: WorkspaceSocket;
  away: boolean;
  place: Place;
  go(place: Place): void;
  openSettings(machine?: MachineId): void;
  /** The workspace selector, at the sidebar's foot. */
  menu: ReactNode;
};

/** The open workspace on its feed: its sections, and its chats or the section `place` names. */
function Workspace(open: Open) {
  const { api, workspace, socket, place } = open;
  const status = useFeed(workspace.id, api, socket);
  // Whether the server serves chats, known once the feed is welcomed.
  const { read, send } = useChatAccess(socket);
  const names = useNames(useMembers(api, workspace.id, socket));
  const served = useSections(socket);
  const searches = useOffers(socket, "workspace.search");
  const palette = (chatMachine?: MachineId) => <Palette api={api} workspace={workspace.id} socket={socket} chatMachine={chatMachine} enabled={searches && !open.away} go={open.go} />;
  if (place.view === "chats") {
    return <WorkspaceChats {...open} place={place} status={status} read={read} send={send} names={names} palette={palette} tasksServed={served.tasks} />;
  }
  return (
    <div className="workspace detail">
      {palette()}
      <div className="side">
        <Sections {...open} />
        {open.menu}
      </div>
      <section>
        <Heading title={TITLES[place.view]} status={status} back={() => open.go({ view: "chats" })} />
        {!served[place.view] ? (
          <p className="page muted">{status.state === "connecting" ? "Loading" : `This server doesn't offer ${TITLES[place.view].toLowerCase()}.`}</p>
        ) : place.view === "tasks" ? (
          <TasksPage api={api} workspace={workspace.id} socket={socket} shown={place.task} startChat={send ? (task) => open.go({ view: "chats", task }) : undefined} />
        ) : place.view === "docs" ? (
          <DocsPage api={api} workspace={workspace.id} socket={socket} names={names} doc={place.doc} choose={(doc) => open.go(doc === undefined ? { view: "docs" } : { view: "docs", doc })} />
        ) : (
          <ProposalsPage api={api} workspace={workspace.id} socket={socket} names={names} />
        )}
      </section>
    </div>
  );
}

// Which of the sections besides chats the server offers on `socket`.
function useSections(socket: WorkspaceSocket) {
  return {
    tasks: useOffers(socket, "workspace.tasks"),
    docs: useOffers(socket, "workspace.docs"),
    proposals: useOffers(socket, "workspace.proposals"),
  };
}

const TITLES = { chats: "Chats", tasks: "Tasks", docs: "Docs", proposals: "Proposals" } as const;

// The workspace's sections the server serves, besides its chats; none when it serves no other.
function Sections({ socket, place, go }: Pick<Open, "socket" | "place" | "go">) {
  const served = useSections(socket);
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

// `back` leaves for the chat list, which a narrow screen shows alone.
function Heading({ title, status, back }: { title: string; status: Status; back(): void }) {
  return (
    <div className="heading">
      <button type="button" className="back" aria-label="Back to chats" onClick={back}>
        Chats
      </button>
      <h1>{title}</h1>
      <p className="status">{statusText(status)}</p>
    </div>
  );
}

/**
 * The workspace's chats, where the server serves them (`read`), kept fresh by its feed, beside the
 * chosen one or a new one. Every member writes in every chat, and calls only their own agent.
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
  send,
  names,
  palette,
  openSettings,
  menu,
  tasksServed,
}: Open & { place: Extract<Place, { view: "chats" }>; status: Status; read: boolean; send: boolean; names: Names; palette(chatMachine?: MachineId): ReactNode; tasksServed: boolean }) {
  const chats = useLoaded(() => (read ? api.chats(workspace.id) : answered({ chats: [] })), `${workspace.id} ${read}`);
  useReloadOn(socket, ({ kind }) => read && kind === "chat_changed", () => void chats.load(), [read]);
  const machines = useLoaded(() => api.machines(), api);
  const shared = useLoaded(() => api.sharedMachines(workspace.id), workspace.id);
  const loadMachines = () => {
    void machines.load();
    void shared.load();
  };
  useReloadOn(socket, ({ kind }) => kind === "machines_changed", () => void shared.load());
  // Back from Settings, where a machine may have been paired or shared, they are read again.
  const wasAway = useRef(away);
  useEffect(() => {
    if (wasAway.current && !away) {
      loadMachines();
    }
    wasAway.current = away;
  }, [away]);
  const others = shared.value?.machines ?? [];
  // Until the feed is welcomed, whether it takes commands isn't known yet.
  const found = machines.value === undefined || status.state === "connecting" ? undefined : readiness(send, machines.value.machines, others);
  const ready = onceSharedKnown(found, shared.value !== undefined);
  const loadedTasks = useTasks(api, workspace.id, socket, tasksServed);
  const tasks = loadedTasks.value?.tasks;
  // New chat for a task opens with it; going to a chat, however, leaves the new one.
  const [starting, setStarting] = useState(place.task !== undefined);
  useEffect(() => setStarting(place.chat === undefined && place.task !== undefined), [place.chat, place.task]);
  // The chat just started here, shown before the list has it, with the agent its first message called.
  const [started, setStarted] = useState<{ id: ChatId; legs: ChatLeg[] }>();
  const live = status.state === "live";
  const listedChat = chats.value?.chats.find(({ id }) => id === place.chat);
  const startedHere = place.chat !== undefined && place.chat === started?.id ? started : undefined;
  const open = listedChat ?? (startedHere && { ...startedHere, title: undefined });
  // The view stays while the feed reconnects; it sends only once the feed is back.
  const own = machines.value === undefined ? undefined : readiness(true, machines.value.machines, others);
  const mine = own?.kind === "ready" ? own.machines : [];
  const missing = <StartMissing ready={ready} problem={machines.problem ?? shared.problem} check={loadMachines} openSettings={openSettings} />;
  // On their own machines, connected or not, and on those shared with the workspace.
  const agents = reached((machines.value?.machines ?? []).map(({ id }) => id), others.map(({ id }) => id), open && { legs: withFirst(open.legs, startedHere?.legs ?? []) });
  const leg = agents[0]?.leg;
  const listed = read ? chats : status.state === "connecting" ? "connecting" : "none";
  return (
    <div className={starting || place.chat !== undefined ? "workspace detail" : "workspace"}>
      {palette(leg?.machine_id)}
      <div className="side">
        <Sections socket={socket} place={place} go={go} />
        <ChatsNav
          loaded={listed}
          names={names}
          chosen={starting ? undefined : place.chat}
          choose={(chat) => {
            setStarting(false);
            go({ view: "chats", chat });
          }}
          start={
            send && status.state !== "connecting"
              ? () => {
                  setStarting(true);
                  loadMachines();
                }
              : undefined
          }
        />
        {menu}
      </div>
      <section>
        <Heading
          title={starting ? "New chat" : (open?.title ?? workspace.name)}
          status={status}
          back={() => {
            setStarting(false);
            go({ view: "chats" });
          }}
        />
        {starting && tasks === undefined && loadedTasks.problem !== undefined && <Problem text={loadedTasks.problem} retry={() => void loadedTasks.load()} />}
        {starting ? (
          <NewChat
            key={place.task}
            socket={socket}
            tasks={tasksServed ? tasks : undefined}
            task={place.task}
            live={live && send}
            away={away}
            machines={mine}
            missing={missing}
            openSettings={openSettings}
            started={(chat, called) => {
              setStarted({ id: chat, legs: firstLeg(called, mine, workspace.member) });
              setStarting(false);
              go({ view: "chats", chat });
              void chats.load();
            }}
          />
        ) : (
          open !== undefined && (
            <>
              {listedChat?.task !== undefined && <ChatTaskLink workspace={workspace.id} task={listedChat.task} tasks={tasks} go={go} />}
              <ChatScreen
                key={open.id}
                link={socket}
                live={live}
                chatRef={{ workspace: workspace.id, chat: open.id }}
                me={workspace.member}
                agents={agents}
                machines={mine}
                missing={missing}
                openSettings={openSettings}
                away={away}
                title={open.title}
                names={names}
                at={place.at}
              />
            </>
          )
        )}
      </section>
    </div>
  );
}
