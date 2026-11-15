import { useEffect, useRef, useState } from "react";

import type { Answer, Api } from "../api/api.ts";
import { type Loaded, Loader } from "../api/loaded.ts";
import { later } from "../connection.ts";
import type { MachineId, SearchHit, Symbol as CodeSymbol, WorkspaceId } from "../protocol.ts";
import type { Place } from "../shell/place.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { composing, picker } from "../composer/keys.ts";
import { type Listed, listed, opensPalette, parts } from "./hits.ts";
import { place, searcher, symbols } from "./symbols.ts";
import "./palette.css";

type Props = {
  api: Api;
  workspace: WorkspaceId;
  socket: WorkspaceSocket;
  /** The open chat's machine, by name, whose code the palette searches first. */
  chatMachine?: string;
  enabled: boolean;
  go(place: Place): void;
};

/**
 * The search palette over `workspace`'s tasks, docs and chats, and the code on one of the person's
 * machines, opened with ⌘K while `enabled`; choosing a hit goes to its place, and a symbol copies where it is.
 */
export function Palette({ enabled, go, ...props }: Props) {
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
      {...props}
      close={close}
      go={(place) => {
        close();
        go(place);
      }}
    />
  );
}

function Search({ api, workspace, socket, chatMachine, close, go }: Omit<Props, "enabled"> & { close(): void }) {
  const [text, setText] = useState("");
  const coded = useSymbols(api, socket, chatMachine, text);
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
  const count = hits.length + coded.symbols.length;
  const choose = (index: number) => {
    const hit: Listed | undefined = hits[index];
    const symbol = coded.symbols[index - hits.length];
    if (hit !== undefined) {
      go(hit.place);
    } else if (symbol !== undefined) {
      void navigator.clipboard.writeText(place(symbol)).catch(() => {});
      close();
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
          aria-expanded={count > 0}
          aria-controls="palette-hits"
          aria-activedescendant={at < count ? `palette-${at}` : undefined}
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
            const action = picker(event.nativeEvent, count, at);
            if (action === "close") {
              close();
            } else if (action === "pick") {
              event.preventDefault();
              choose(at);
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
              onClick={() => choose(index)}
            >
              <span className="muted">{hit.kind}</span> <span className="title">{hit.title}</span>
              <span className="snippet">
                {parts(hit.snippet).map((part, cut) => (part.marked ? <mark key={cut}>{part.text}</mark> : <span key={cut}>{part.text}</span>))}
              </span>
            </li>
          ))}
          {(coded.symbols.length > 0 || coded.problem !== undefined) && (
            <li role="group" aria-label="Symbols">
              <span className="muted">Symbols</span>
              {coded.problem !== undefined && <span role="alert">{coded.problem}</span>}
              <ul role="none">
                {coded.symbols.map((symbol, cut) => {
                  const index = hits.length + cut;
                  return (
                    <li
                      key={`${symbol.repo}:${symbol.name}:${place(symbol)}`}
                      id={`palette-${index}`}
                      role="option"
                      aria-selected={index === at}
                      onMouseEnter={() => setAt(index)}
                      onClick={() => choose(index)}
                    >
                      <span className="muted">{symbol.kind}</span> <span className="title">{symbol.name}</span>
                      <span className="snippet">
                        {symbol.repo} · {place(symbol)}
                      </span>
                    </li>
                  );
                })}
              </ul>
            </li>
          )}
        </ul>
        {answered && count === 0 && coded.settled && <p className="muted">Nothing found.</p>}
      </div>
    </div>
  );
}

type Answered = { text: string; hits: SearchHit[] };

type Coded = { text: string; symbols: CodeSymbol[] };

/**
 * The symbols on the person's machine that match `text`, once its answer is for that text; none
 * while no machine of theirs offers them. `settled` once nothing is left to wait for.
 */
function useSymbols(api: Api, socket: WorkspaceSocket, chatMachine: string | undefined, text: string) {
  const offered = useOffers(socket, "symbols.search");
  const [machine, setMachine] = useState<MachineId | undefined>(undefined);
  const [asked, setAsked] = useState(false);
  const [coded, setCoded] = useState<Loaded<Coded>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<Coded>(setCoded));
  useEffect(() => {
    let live = true;
    if (offered) {
      void api.machines().then((answer) => {
        if (live) {
          setMachine(answer.ok ? searcher(answer.value.machines, chatMachine) : undefined);
          setAsked(true);
        }
      });
    }
    return () => {
      live = false;
    };
  }, [api, offered, chatMachine]);
  useEffect(() => () => loader.stop(), [loader]);
  useEffect(() => {
    if (machine !== undefined && text.trim() !== "") {
      void loader.load(async () => {
        const answer = await symbols(socket, machine, text, later);
        return answer.ok ? { ok: true, value: { text, symbols: answer.value } } : answer;
      });
    }
  }, [loader, socket, machine, text]);
  const answered = coded.value?.text === text;
  return {
    symbols: machine !== undefined && answered ? (coded.value?.symbols ?? []) : [],
    problem: machine === undefined ? undefined : coded.problem,
    settled: !offered || (asked && (machine === undefined || answered || coded.problem !== undefined)),
  };
}

async function search(api: Api, workspace: WorkspaceId, text: string): Promise<Answer<Answered>> {
  const answer = await api.search(workspace, { text });
  return answer.ok ? { ok: true, value: { text, hits: answer.value.hits } } : answer;
}
