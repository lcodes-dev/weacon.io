import { type KeyboardEvent, type RefObject, useEffect, useLayoutEffect, useRef, useState } from "react";

import "./composer.css";
import { type Draft, type MentionItem, type Placed, called, editDraft, opening, pickMention, query, unwritten } from "./draft.ts";
import { composing, picker, sends } from "./keys.ts";
import { offered } from "./mentionables.ts";
import type { Send, Sent } from "./prompt.ts";
import { useSend } from "./send.ts";

/**
 * The message box. While `blocked` says why, it still takes text, but can't send it. It keeps the
 * text until its machine accepts it, and says so when it doesn't, as `explain` words it. Typing `@`
 * offers `agents`, those the writer may call in this chat, and a message that mentions one calls
 * it. While `calling`, the box opens on the first one's mention, so a dialogue with it needs none
 * typed.
 */
export function Composer({
  send,
  blocked,
  hidden,
  explain,
  agents,
  calling,
}: {
  send: Send;
  blocked: string | undefined;
  hidden: boolean;
  explain?: (sent: Sent) => string | undefined;
  agents: readonly MentionItem[];
  /** Whether the writer's messages here go to an agent unless they say otherwise. */
  calling: boolean;
}) {
  const agent = agents[0];
  const [placed, setPlaced] = useState<Placed>(() => opening(agent, calling));
  const opens = useRef(() => opening(agent, calling));
  opens.current = () => opening(agent, calling);
  // A box nothing was written in follows where the writer's messages now go.
  useEffect(() => setPlaced((now) => (unwritten(now.draft) ? opens.current() : now)), [agent?.id, agent?.label, calling]);
  const [active, setActive] = useState(0);
  const [dismissed, setDismissed] = useState<number>();
  // A draft edited after it was sent stays in the box.
  const { sending, problem, submit: deliver, clear } = useSend<string>(
    send,
    (sent) => setPlaced((now) => (now.draft.text === sent ? opens.current() : now)),
    explain,
  );
  const box = useRef<HTMLTextAreaElement>(null);
  const backdrop = useRef<HTMLDivElement>(null);
  const { draft, caret } = placed;
  const at = query(draft, caret);
  const options = offered(at, dismissed, sending, agents);
  const calls = called(draft, agents)?.id;
  // A mention of an agent the writer can't call now would otherwise go as a plain message.
  const stale = draft.mentions.length > 0 && calls === undefined ? "The agent you mention can't be called now. Remove the mention, or pick your agent again." : undefined;
  const hint = problem ?? blocked ?? stale;
  const ready = !sending && blocked === undefined && stale === undefined && !unwritten(draft);

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
      await deliver(draft.text, draft.text, calls);
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
          placeholder={agent === undefined ? "Write to the chat" : `Write to the chat, or type @ to call ${agents.length === 1 ? agent.label : "an agent"}`}
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
    <ul className="mentions" id="mentions" role="listbox" aria-label="Agents">
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
          <span className="muted">{item.note}</span>
        </li>
      ))}
    </ul>
  );
}
