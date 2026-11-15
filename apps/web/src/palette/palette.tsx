import { useEffect, useRef, useState } from "react";

import type { Api } from "../api/api.ts";
import { type Loaded, Loader } from "../api/loaded.ts";
import type { ChatId, SearchAnswer, WorkspaceId } from "../protocol.ts";
import { type Listed, listed, moved, opensPalette, parts } from "./hits.ts";
import "./palette.css";

/**
 * The search palette over `workspace`'s tasks, docs and chats, opened with ⌘K while `enabled`.
 * Choosing a chat opens it; tasks and docs have no page here yet, so they are only listed.
 */
export function Palette({ api, workspace, enabled, openChat }: { api: Api; workspace: WorkspaceId; enabled: boolean; openChat(chat: ChatId): void }) {
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
      openChat={(chat) => {
        close();
        openChat(chat);
      }}
    />
  );
}

function Search({ api, workspace, close, openChat }: { api: Api; workspace: WorkspaceId; close(): void; openChat(chat: ChatId): void }) {
  const [text, setText] = useState("");
  const [found, setFound] = useState<Loaded<SearchAnswer>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<SearchAnswer>(setFound));
  const [at, setAt] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    return () => loader.stop();
  }, [loader]);
  const hits = text.trim() === "" ? [] : listed(found.value?.hits ?? []);
  const choose = (hit: Listed | undefined) => {
    if (hit?.chat !== undefined) {
      openChat(hit.chat);
    }
  };
  return (
    <div className="palette-backdrop" onClick={close}>
      <div role="dialog" aria-label="Search" className="palette" onClick={(event) => event.stopPropagation()}>
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
              void loader.load(() => api.search(workspace, { text: typed }));
            }
          }}
          onKeyDown={(event) => {
            if (event.key === "Escape") {
              close();
            } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              setAt(moved(at, hits.length, event.key === "ArrowDown" ? 1 : -1));
            } else if (event.key === "Enter") {
              choose(hits[at]);
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
              aria-disabled={hit.chat === undefined}
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
        {text.trim() !== "" && found.value !== undefined && hits.length === 0 && <p className="muted">Nothing found.</p>}
      </div>
    </div>
  );
}
