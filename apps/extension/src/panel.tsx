import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";

import { working } from "./chat.ts";
import { say } from "./feedback.ts";
import { useLive } from "./live.ts";
import { pickOn } from "./pick.ts";
import type { Pick } from "./protocol.ts";
import type { ChatRef } from "./sync.ts";
import { choose, chosen } from "./tabs.ts";
import { codeOf, tell } from "./tell.ts";
import { Attached, ChatList, Transcript } from "./views.tsx";

function Panel() {
  const tab = useActiveTab();
  const [ref, setRef] = useState<ChatRef>();
  const live = useLive(ref);
  const [text, setText] = useState("");
  const [pick, setPick] = useState<Pick>();
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState<string>();

  useEffect(() => {
    setRef(tab === undefined ? undefined : chosen(localStorage, tab));
    setPick(undefined);
  }, [tab]);

  const open = async (next: ChatRef | undefined) => {
    setRef(next);
    setFailed(undefined);
    setPick(undefined);
    if (tab !== undefined && next !== undefined) {
      const tabs = await chrome.tabs.query({});
      choose(localStorage, tab, next, tabs.flatMap((each) => (each.id === undefined ? [] : [each.id])));
    }
  };
  // Runs `work`, showing what stopped it; one at a time.
  const attempt = async (work: () => Promise<void>) => {
    setBusy(true);
    setFailed(undefined);
    try {
      await work();
    } catch (error: unknown) {
      const code = codeOf(error);
      tell(code);
      setFailed(code);
    } finally {
      setBusy(false);
    }
  };
  const pickElement = () =>
    attempt(async () => {
      const [active] = await chrome.tabs.query({ active: true, currentWindow: true });
      const picked = active === undefined ? undefined : await pickOn(active);
      if (picked !== undefined) {
        setPick(picked);
      }
    });
  const send = () =>
    attempt(async () => {
      await live.prompt(text, pick);
      setText("");
      setPick(undefined);
    });

  const ready = live.status.kind === "ready";
  const shown = live.sync !== undefined && ref !== undefined ? live.sync : undefined;
  const stale = shown?.chat !== undefined && shown.state.kind === "failed" ? shown.state.code : undefined;
  const problem = failed ?? live.refused ?? (live.status.kind === "failed" ? live.status.code : stale);
  return (
    <main>
      <header>
        {ref === undefined ? <h1>Chats</h1> : <button type="button" onClick={() => void open(undefined)}>All chats</button>}
        {live.status.kind === "connecting" ? <span className="quiet">Connecting…</span> : null}
      </header>
      {problem === undefined ? null : (
        <p className="problem" role="alert">
          {say(problem)}{" "}
          {live.status.kind === "failed" ? (
            <button type="button" onClick={live.reconnect}>
              Try again
            </button>
          ) : null}
        </p>
      )}
      {ref === undefined ? (
        live.chats === undefined ? null : <ChatList chats={live.chats} choose={(next) => void open(next)} />
      ) : shown === undefined ? (
        <p className="quiet">Loading…</p>
      ) : (
        <Transcript
          sync={shown}
          answer={(line, allow) => void attempt(() => live.command({ kind: "permission", answer: { run: line.run, prompt: line.prompt, allow } }))}
        />
      )}
      {ref === undefined ? null : (
        <form
          className="composer"
          onSubmit={(event) => {
            event.preventDefault();
            void send();
          }}
        >
          {pick === undefined ? null : <Attached pick={pick} remove={() => setPick(undefined)} />}
          <textarea value={text} placeholder="Ask for a change" rows={3} onChange={(event) => setText(event.target.value)} />
          <p className="actions">
            <button type="button" disabled={busy || !ready} onClick={() => void pickElement()}>
              Pick an element
            </button>
            {shown?.chat !== undefined && working(shown.chat) ? (
              <button type="button" disabled={!ready} onClick={() => void attempt(() => live.command({ kind: "interrupt" }))}>
                Stop
              </button>
            ) : null}
            <button type="submit" disabled={busy || !ready || text.trim() === ""}>
              Send
            </button>
          </p>
        </form>
      )}
    </main>
  );
}

// The id of the tab showing in this window, as it changes.
function useActiveTab(): number | undefined {
  const [tab, setTab] = useState<number>();
  useEffect(() => {
    const look = async () => setTab((await chrome.tabs.query({ active: true, currentWindow: true }))[0]?.id);
    void look();
    const changed = () => void look();
    chrome.tabs.onActivated.addListener(changed);
    chrome.windows.onFocusChanged.addListener(changed);
    return () => {
      chrome.tabs.onActivated.removeListener(changed);
      chrome.windows.onFocusChanged.removeListener(changed);
    };
  }, []);
  return tab;
}

const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(<Panel />);
}
