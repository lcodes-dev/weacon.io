import { useEffect, useRef, useState } from "react";

import type { Answer, Api } from "../api/api.ts";
import { type Loaded, Loader } from "../api/loaded.ts";
import { problemText } from "../api/problems.ts";
import { later } from "../connection.ts";
import type { MachineId, SearchHit, Symbol as CodeSymbol, WorkspaceId } from "../protocol.ts";
import type { Place } from "../shell/place.ts";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { composing, picker } from "../composer/keys.ts";
import { type Listed, listed, opensPalette, parts } from "./hits.ts";
import { SYMBOLS, place, searcher, symbols } from "./symbols.ts";
import "./palette.css";

type Props = {
  api: Api;
  workspace: WorkspaceId;
  socket: WorkspaceSocket;
  /** The open chat's machine, by name, whose code the palette searches first. */
  chatMachine?: string | undefined;
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
  // The chosen result by its key, so results that arrive later never move the choice.
  const [chosen, setChosen] = useState<string | undefined>(undefined);
  const [copyProblem, setCopyProblem] = useState<string | undefined>(undefined);
  // Each copy's number; typing starts a new one, so a copy that ends late changes nothing.
  const copying = useRef(0);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    input.current?.focus();
    return () => {
      loader.stop();
      // A copy that ends once this palette is gone changes nothing in the next one.
      copying.current += 1;
    };
  }, [loader]);
  const asked = text.trim() !== "";
  const answered = asked && found.value?.text === text;
  const hits = answered ? listed(found.value?.hits ?? []) : [];
  const keys = [...hits.map((hit) => hit.key), ...coded.symbols.map(symbolKey)];
  const count = keys.length;
  const at = Math.max(0, keys.indexOf(chosen ?? ""));
  const setAt = (index: number) => setChosen(keys[index]);
  useEffect(() => {
    document.getElementById(`palette-${at}`)?.scrollIntoView({ block: "nearest" });
  }, [at, found]);
  const choose = (index: number) => {
    const hit: Listed | undefined = hits[index];
    const symbol = coded.symbols[index - hits.length];
    if (hit !== undefined) {
      go(hit.place);
    } else if (symbol !== undefined) {
      const copy = ++copying.current;
      const current = () => copy === copying.current;
      navigator.clipboard.writeText(place(symbol)).then(
        () => current() && close(),
        () => current() && setCopyProblem("The place couldn't be copied."),
      );
    }
  };
  return (
    <div className="palette-backdrop" onClick={close}>
      <div role="dialog" aria-modal="true" aria-label="Search" className="palette" onClick={(event) => event.stopPropagation()}>
        <input
          ref={input}
          aria-label="Search"
          placeholder={coded.offered ? "Search tasks, docs, chats and code" : "Search tasks, docs and chats"}
          role="combobox"
          aria-expanded={count > 0}
          aria-controls="palette-hits"
          aria-activedescendant={at < count ? `palette-${at}` : undefined}
          value={text}
          onChange={(event) => {
            const typed = event.target.value;
            setText(typed);
            setChosen(undefined);
            copying.current += 1;
            setCopyProblem(undefined);
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
        {copyProblem !== undefined && <p role="alert">{copyProblem}</p>}
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
                      key={symbolKey(symbol)}
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

type Coded = { machine: MachineId; text: string; symbols: CodeSymbol[] };

const symbolKey = (symbol: CodeSymbol) => `symbol:${symbol.repo}:${symbol.name}:${place(symbol)}`;

/**
 * The symbols on the person's machine that match `text`, once its answer is for that text; none
 * while no machine of theirs offers them. `settled` once nothing is left to wait for.
 */
function useSymbols(api: Api, socket: WorkspaceSocket, chatMachine: string | undefined, text: string) {
  const offered = useOffers(socket, SYMBOLS);
  // Undefined until the machines are listed, null when none of them offers symbols.
  const [machine, setMachine] = useState<MachineId | null | undefined>(undefined);
  // Why the machines couldn't be listed; the palette lists them again when it next opens.
  const [unlisted, setUnlisted] = useState<string | undefined>(undefined);
  const [coded, setCoded] = useState<Loaded<Coded>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<Coded>(setCoded));
  useEffect(() => {
    let live = true;
    if (offered) {
      void api.machines().then((answer) => {
        if (live) {
          setMachine((answer.ok ? searcher(answer.value.machines, chatMachine) : undefined) ?? null);
          setUnlisted(answer.ok ? undefined : problemText(answer.code));
        }
      });
    }
    return () => {
      live = false;
    };
  }, [api, offered, chatMachine]);
  useEffect(() => () => loader.stop(), [loader]);
  useEffect(() => {
    if (typeof machine === "string" && text.trim() !== "") {
      void loader.load(async () => {
        const answer = await symbols(socket, machine, text, later);
        return answer.ok ? { ok: true, value: { machine, text, symbols: answer.value } } : answer;
      });
    }
  }, [loader, socket, machine, text]);
  const asking = typeof machine === "string";
  const answered = asking && coded.value?.machine === machine && coded.value.text === text;
  return {
    symbols: answered ? (coded.value?.symbols ?? []) : [],
    problem: asking ? coded.problem : unlisted,
    settled: !offered || machine === null || answered || coded.problem !== undefined,
    offered,
  };
}

async function search(api: Api, workspace: WorkspaceId, text: string): Promise<Answer<Answered>> {
  const answer = await api.search(workspace, { text });
  return answer.ok ? { ok: true, value: { text, hits: answer.value.hits } } : answer;
}
