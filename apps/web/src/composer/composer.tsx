import { type KeyboardEvent, type RefObject, useLayoutEffect, useRef, useState } from "react";

import "./composer.css";
import { type Draft, EMPTY, type Item, type Placed, edit, pick, query } from "./draft.ts";
import { composing, picker, sends } from "./keys.ts";
import { offered } from "./mentionables.ts";
import { type Retry, type Send, after, requestFor, requestId } from "./prompt.ts";

/**
 * The message box. While `blocked` says why, it still takes text, but can't send it. It keeps the
 * text until the server accepts it, and says so when it doesn't.
 */
export function Composer({ send, blocked, hidden }: { send: Send | undefined; blocked: string | undefined; hidden: boolean }) {
  const [placed, setPlaced] = useState<Placed>({ draft: EMPTY, caret: 0 });
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<number>();
  const [sending, setSending] = useState(false);
  const [problem, setProblem] = useState<string>();
  const retry = useRef<Retry>(undefined);
  const box = useRef<HTMLTextAreaElement>(null);
  const backdrop = useRef<HTMLDivElement>(null);
  const { draft, caret } = placed;
  const at = query(draft, caret);
  const options = offered(at, dismissed, sending);
  const hint = problem ?? blocked;
  const ready = !sending && blocked === undefined && send !== undefined && draft.text.trim() !== "";

  useLayoutEffect(() => {
    const element = box.current;
    if (element !== null && document.activeElement === element && element.selectionEnd !== caret) {
      element.setSelectionRange(caret, caret);
    }
  }, [placed]);

  const place = (next: Placed) => {
    setPlaced(next);
    setActive(0);
    if (query(next.draft, next.caret)?.start !== dismissed) {
      setDismissed(undefined);
    }
  };
  const submit = async () => {
    if (!ready) {
      return;
    }
    const { text } = draft;
    const request = requestFor(retry.current, text, () => requestId(crypto.getRandomValues(new Uint8Array(16))));
    setSending(true);
    setProblem(undefined);
    const next = after(await send(text, request), text, request);
    setSending(false);
    setProblem(next.problem);
    retry.current = next.retry;
    if (next.text === "") {
      place({ draft: EMPTY, caret: 0 });
    }
  };
  const choose = (item: Item | undefined) => {
    if (at !== undefined && item !== undefined) {
      place(pick(draft, at, item));
    }
  };
  const keyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    const key = event.nativeEvent;
    const picking = at === undefined || options.length === 0 || composing(key) ? undefined : picker(key, options.length, active);
    if (picking === undefined) {
      if (sends(key)) {
        event.preventDefault();
        void submit();
      }
      return;
    }
    event.preventDefault();
    if (picking === "pick") {
      choose(options[active]);
    } else if (picking === "close") {
      setDismissed(at?.start);
    } else {
      setActive(picking);
    }
  };

  return (
    <form
      className="composer"
      hidden={hidden}
      onSubmit={(event) => {
        event.preventDefault();
        void submit();
      }}
    >
      <div className="field">
        <Backdrop draft={draft} ref={backdrop} />
        <textarea
          ref={box}
          aria-label="Message"
          role="combobox"
          aria-expanded={options.length > 0}
          aria-controls="mentions"
          aria-activedescendant={options.length > 0 ? `mention-${active}` : undefined}
          rows={2}
          value={draft.text}
          readOnly={sending}
          onChange={(event) => {
            place(edit(draft, event.target.value, event.target.selectionEnd, (event.nativeEvent as InputEvent).isComposing));
            setProblem(undefined);
          }}
          onSelect={(event) => {
            const end = event.currentTarget.selectionEnd;
            if (end !== caret) {
              place({ draft, caret: end });
            }
          }}
          onScroll={(event) => backdrop.current?.scrollTo(0, event.currentTarget.scrollTop)}
          onKeyDown={keyDown}
        />
      </div>
      <button type="submit" disabled={!ready}>
        Send
      </button>
      {options.length > 0 && <Options options={options} active={active} choose={choose} />}
      {hint !== undefined && <p className="hint muted">{hint}</p>}
    </form>
  );
}

// The draft's text behind the box, so its mentions show; the box's own text is drawn over it.
function Backdrop({ draft, ref }: { draft: Draft; ref: RefObject<HTMLDivElement | null> }) {
  const pieces = [];
  let from = 0;
  for (const { start, end } of draft.mentions) {
    pieces.push(draft.text.slice(from, start), <mark key={start}>{draft.text.slice(start, end)}</mark>);
    from = end;
  }
  // A trailing newline needs a character after it to take up its line.
  pieces.push(`${draft.text.slice(from)} `);
  return (
    <div className="backdrop" aria-hidden="true" ref={ref}>
      {pieces}
    </div>
  );
}

function Options({
  options,
  active,
  choose,
}: {
  options: Item[];
  active: number;
  choose: (item: Item) => void;
}) {
  return (
    <ul className="mentions" id="mentions" role="listbox" aria-label="People and agents">
      {options.map((item, index) => (
        <li
          key={item.id}
          id={`mention-${index}`}
          role="option"
          aria-selected={index === active}
          // Picked on mouse down, so the box keeps its focus and caret.
          onMouseDown={(event) => {
            event.preventDefault();
            choose(item);
          }}
        >
          {item.label}
          <span className="muted">{item.kind === "agent" ? "Agent" : "Person"}</span>
        </li>
      ))}
    </ul>
  );
}
