import { type Line, lines } from "./chat.ts";
import { say } from "./feedback.ts";
import type { LocalChat, Pick, RunState } from "./protocol.ts";
import type { ChatRef, Sync } from "./sync.ts";

const STATES: Record<RunState, string> = {
  starting: "Starting",
  running: "Working",
  waiting: "Needs you",
  idle: "Idle",
  sleeping: "Idle",
  ended: "Ended",
};

/** This machine's chats, each workspace's together, newest first. */
export function ChatList({ chats, choose }: { chats: readonly LocalChat[]; choose: (ref: ChatRef) => void }) {
  if (chats.length === 0) {
    return <p className="quiet">No chats on this machine yet. Start one in Weacon.</p>;
  }
  const workspaces = [...new Set(chats.map((chat) => chat.workspace))];
  return (
    <nav className="chats">
      {workspaces.map((workspace) => (
        <section key={workspace}>
          <h2>{workspace}</h2>
          <ul>
            {chats
              .filter((chat) => chat.workspace === workspace)
              .map((chat) => (
                <li key={chat.chat}>
                  <button type="button" onClick={() => choose({ workspace, chat: chat.chat })}>
                    <span className="title">{chat.title === "" ? "Untitled chat" : chat.title}</span>
                    <span className="state">{chat.run === undefined ? "New" : STATES[chat.run]}</span>
                  </button>
                </li>
              ))}
          </ul>
        </section>
      ))}
    </nav>
  );
}

/** The chat's transcript, with its waiting permission prompts answerable in place. */
export function Transcript({ sync, answer }: { sync: Sync; answer: (line: Extract<Line, { kind: "ask" }>, allow: boolean) => void }) {
  if (sync.chat === undefined) {
    return <p className="quiet">{sync.state.kind === "failed" ? say(sync.state.code) : "Loading…"}</p>;
  }
  return (
    <ol className="transcript">
      {lines(sync.chat).map((line) => (
        <li key={line.key} className={line.kind}>
          <Row line={line} answer={answer} />
        </li>
      ))}
    </ol>
  );
}

function Row({ line, answer }: { line: Line; answer: (line: Extract<Line, { kind: "ask" }>, allow: boolean) => void }) {
  switch (line.kind) {
    case "prompt":
      return (
        <>
          <p>{line.text}</p>
          {line.pick === undefined ? null : (
            <p className="note">
              Picked <code>{line.pick.selector}</code> on {place(line.pick)}
            </p>
          )}
        </>
      );
    case "text":
      return <p>{line.text}</p>;
    case "tool":
      return (
        <p>
          <code>{line.tool}</code> {line.state === "running" ? "…" : line.state === "failed" ? "failed" : "done"}
        </p>
      );
    case "ask":
      return (
        <div>
          <p>
            The agent asks to use <code>{line.tool === "" ? "a tool" : line.tool}</code>.
          </p>
          {line.state === "open" && line.wants !== undefined && <pre className="wants">{line.wants.text}</pre>}
          {line.state === "open" && line.wants?.cut === true && <p className="quiet">The rest is too long to show.</p>}
          {line.state === "open" ? (
            <p className="actions">
              <button type="button" className="primary" onClick={() => answer(line, true)}>
                Allow
              </button>
              <button type="button" onClick={() => answer(line, false)}>
                Deny
              </button>
            </p>
          ) : (
            <p className="quiet">{line.state === "allowed" ? "Allowed" : line.state === "denied" ? "Denied" : "No longer waiting"}</p>
          )}
        </div>
      );
    case "ended":
      return <p className="quiet">{say(line.code)}</p>;
  }
}

// A page by its title, or its address when it has none.
function place({ title, url }: { title: string; url: string }): string {
  return title === "" ? url : title;
}

/** The pick going with the next message: its screenshot, selector and page, until removed. */
export function Attached({ pick, remove }: { pick: Pick; remove: () => void }) {
  return (
    <figure className="pick">
      {pick.screenshot_png === undefined ? null : <img alt="The picked element" src={`data:image/png;base64,${pick.screenshot_png}`} />}
      <figcaption>
        <code>{pick.selector}</code>
        <span>{place(pick)}</span>
        {pick.component === undefined ? null : <span>{pick.component}</span>}
      </figcaption>
      <button type="button" aria-label="Remove the picked element" onClick={remove}>
        ×
      </button>
    </figure>
  );
}
