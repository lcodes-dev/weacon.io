import { useEffect, useRef, useState } from "react";

import type { Answer, Api } from "../api/api.ts";
import { type Loaded, Loader } from "../api/loaded.ts";
import type { SearchHit, WorkspaceId } from "../protocol.ts";
import type { Place } from "../shell/place.ts";
import { composing, picker } from "../composer/keys.ts";
import { type Listed, listed, opensPalette, parts } from "./hits.ts";
import "./palette.css";

/** The search palette over `workspace`'s tasks, docs and chats, opened with ⌘K while `enabled`; choosing a hit goes to its place. */
export function Palette({ api, workspace, enabled, go }: { api: Api; workspace: WorkspaceId; enabled: boolean; go(place: Place): void }) {
  const [open, setOpen] = useState(false);
  useEffect(() => {
    if (!enabled) {
      setOpen(false);
      return;
    }
    const opening = (event: KeyboardEvent) => {
      if (opensPalette(event)) {
        event.preventDefault();
        setOpen(true);
      }
    };
    window.addEventListener("keydown", opening);
    return () => window.removeEventListener("keydown", opening);
  }, [enabled]);
  if (!open) {
    return null;
  }
  const close = () => setOpen(false);
  return (
    <Search
      api={api}
      workspace={workspace}
      close={close}
      go={(place) => {
        close();
        go(place);
      }}
    />
  );
}

function Search({ api, workspace, close, go }: { api: Api; workspace: WorkspaceId; close(): void; go(place: Place): void }) {
  const [text, setText] = useState("");
  // Each answer keeps the text it is for, so a slow one never lists, or opens, for text since changed.
  const [found, setFound] = useState<Loaded<Answered>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<Answered>(setFound));
  const [at, setAt] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    return () => loader.stop();
  }, [loader]);
  useEffect(() => {
    document.getElementById(`palette-${at}`)?.scrollIntoView({ block: "nearest" });
  }, [at, found]);
  const asked = text.trim() !== "";
  const answered = asked && found.value?.text === text;
  const hits = answered ? listed(found.value?.hits ?? []) : [];
  const choose = (hit: Listed | undefined) => {
    if (hit !== undefined) {
      go(hit.place);
    }
  };
  return (
    <div className="palette-backdrop" onClick={close}>
      <div role="dialog" aria-modal="true" aria-label="Search" className="palette" onClick={(event) => event.stopPropagation()}>
        <input
          ref={input}
          aria-label="Search"
          placeholder="Search tasks, docs and chats"
          role="combobox"
          aria-expanded={hits.length > 0}
          aria-controls="palette-hits"
          aria-activedescendant={hits[at] === undefined ? undefined : `palette-${at}`}
          value={text}
          onChange={(event) => {
            const typed = event.target.value;
            setText(typed);
            setAt(0);
            if (typed.trim() !== "") {
              void loader.load(() => search(api, workspace, typed));
            }
          }}
          onKeyDown={(event) => {
            if (composing(event.nativeEvent)) {
              return;
            }
            // The input is the palette's only stop, so Tab stays on it.
            if (event.key === "Tab") {
              event.preventDefault();
              return;
            }
            const action = picker(event.nativeEvent, hits.length, at);
            if (action === "close") {
              close();
            } else if (action === "pick") {
              event.preventDefault();
              choose(hits[at]);
            } else if (action !== undefined) {
              event.preventDefault();
              setAt(action);
            }
          }}
        />
        {found.problem !== undefined && <p role="alert">{found.problem}</p>}
        <ul id="palette-hits" role="listbox" aria-label="Results">
          {hits.map((hit, index) => (
            <li
              key={hit.key}
              id={`palette-${index}`}
              role="option"
              aria-selected={index === at}
              onMouseEnter={() => setAt(index)}
              onClick={() => choose(hit)}
            >
              <span className="muted">{hit.kind}</span> <span className="title">{hit.title}</span>
              <span className="snippet">
                {parts(hit.snippet).map((part, cut) => (part.marked ? <mark key={cut}>{part.text}</mark> : <span key={cut}>{part.text}</span>))}
              </span>
            </li>
          ))}
        </ul>
        {answered && hits.length === 0 && <p className="muted">Nothing found.</p>}
      </div>
    </div>
  );
}

type Answered = { text: string; hits: SearchHit[] };

async function search(api: Api, workspace: WorkspaceId, text: string): Promise<Answer<Answered>> {
  const answer = await api.search(workspace, { text });
  return answer.ok ? { ok: true, value: { text, hits: answer.value.hits } } : answer;
}
