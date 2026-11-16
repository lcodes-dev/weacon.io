import { type KeyboardEvent, type RefObject, useLayoutEffect, useRef, useState } from "react";

import "./composer.css";
import { type Draft, EMPTY_DRAFT, type MentionItem, type Placed, editDraft, pickMention, query } from "./draft.ts";
import { composing, picker, sends } from "./keys.ts";
import { offered } from "./mentionables.ts";
import type { Send, Sent } from "./prompt.ts";
import { useSend } from "./send.ts";

/**
 * The message box. While `blocked` says why, it still takes text, but can't send it. It keeps the
 * text until its machine accepts it, and says so when it doesn't, as `explain` words it. Typing `@`
 * offers `agent`, the writer's own in this chat, and a message that mentions it calls it.
 */
export function Composer({
  send,
  blocked,
  hidden,
  explain,
  agent,
}: {
  send: Send;
  blocked: string | undefined;
  hidden: boolean;
  explain?: (sent: Sent) => string | undefined;
  agent: MentionItem | undefined;
}) {
  const [placed, setPlaced] = useState<Placed>({ draft: EMPTY_DRAFT, caret: 0 });
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<number>();
  // A draft edited after it was sent stays in the box.
  const { sending, problem, submit: deliver, clear } = useSend<string>(
    send,
    (sent) => setPlaced((now) => (now.draft.text === sent ? { draft: EMPTY_DRAFT, caret: 0 } : now)),
    explain,
  );
  const box = useRef<HTMLTextAreaElement>(null);
  const backdrop = useRef<HTMLDivElement>(null);
  const { draft, caret } = placed;
  const at = query(draft, caret);
  const options = offered(at, dismissed, sending, agent);
  const hint = problem ?? blocked;
  const ready = !sending && blocked === undefined && draft.text.trim() !== "";

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
    if (ready) {
      await deliver(draft.text, draft.text, draft.mentions.length > 0);
    }
  };
  const choose = (item: MentionItem | undefined) => {
    if (at !== undefined && item !== undefined) {
      place(pickMention(draft, at, item));
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
          placeholder={agent === undefined ? "Write to the chat" : `Write to the chat, or type @ to call ${agent.label}`}
          role="combobox"
          aria-expanded={options.length > 0}
          aria-controls="mentions"
          aria-activedescendant={options.length > 0 ? `mention-${active}` : undefined}
          rows={2}
          value={draft.text}
          readOnly={sending}
          onChange={(event) => {
            place(editDraft(draft, event.target.value, event.target.selectionEnd, (event.nativeEvent as InputEvent).isComposing));
            clear();
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
  options: MentionItem[];
  active: number;
  choose: (item: MentionItem) => void;
}) {
  return (
    <ul className="mentions" id="mentions" role="listbox" aria-label="Your agent">
      {options.map((item, index) => (
        <li
          key={item.id}
          id={`mention-${index}`}
          role="option"
          aria-selected={index === active}
          // The box keeps its focus and caret; a click picks, as assistive technology sends it.
          onMouseDown={(event) => event.preventDefault()}
          onClick={() => choose(item)}
        >
          {item.label}
          <span className="muted">Your agent</span>
        </li>
      ))}
    </ul>
  );
}
