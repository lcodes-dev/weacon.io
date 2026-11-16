import { useEffect, useMemo, useRef, useState } from "react";

import type { Later } from "../connection.ts";
import type { MachineSummary, OutsideSession } from "../protocol.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { vendorName } from "../chat/label.ts";
import { type Line, lines } from "../chat/lines.ts";
import { TranscriptView } from "../chat/transcript-view.tsx";
import type { MachineSocket } from "./ask-machine.ts";
import { type Read, readOn, sessions } from "./outside.ts";

/** The Claude Code and Codex sessions the person ran on `machine` themselves, and one opened to read. */
export function OutsideSessions({ machine, socket, later }: { machine: MachineSummary; socket: MachineSocket; later: Later }) {
  const [open, setOpen] = useState<OutsideSession>();
  const listed = useLoaded(() => sessions(socket, machine.id, later), machine.id);
  if (open !== undefined) {
    return <OutsideSessionView machine={machine} socket={socket} later={later} session={open} close={() => setOpen(undefined)} />;
  }
  return (
    <section aria-label={`${machine.name} sessions`}>
      <h4>Sessions outside Weacon</h4>
      <p className="muted">The Claude Code and Codex sessions you ran on this machine yourself. Only you can see them, and only to read.</p>
      {listed.loading && <p className="muted">Loading</p>}
      {listed.problem !== undefined && <Problem text={listed.problem} retry={() => void listed.load()} />}
      {listed.value?.length === 0 && <p className="muted">No sessions yet.</p>}
      {listed.value !== undefined && listed.value.length > 0 && (
        <ul aria-label="Sessions">
          {listed.value.map((session) => (
            <li key={`${session.agent} ${session.session}`}>
              <button type="button" onClick={() => setOpen(session)}>
                {title(session)}
              </button>{" "}
              <span className="muted">{new Date(session.modified_at * 1000).toLocaleString()}</span>
            </li>
          ))}
        </ul>
      )}
      {listed.value !== undefined && (
        <button type="button" onClick={() => void listed.load()}>
          Refresh
        </button>
      )}
    </section>
  );
}

function OutsideSessionView({
  machine,
  socket,
  later,
  session,
  close,
}: {
  machine: MachineSummary;
  socket: MachineSocket;
  later: Later;
  session: OutsideSession;
  close(): void;
}) {
  const held = useRef<Read>(undefined);
  const shownNow = useRef(true);
  useEffect(() => {
    shownNow.current = true;
    return () => {
      shownNow.current = false;
    };
  }, []);
  const opened = useLoaded(
    () => readOn(socket, machine.id, session, held.current, later, () => shownNow.current),
    `${machine.id} ${session.agent} ${session.session}`,
  );
  held.current = opened.value;
  const chat = opened.value?.chat;
  const shown = useMemo(() => (chat === undefined ? [] : named(lines(chat), vendorName(session.agent))), [chat, session.agent]);
  return (
    <section aria-label={title(session)}>
      <h4>{title(session)}</h4>
      <p>
        <button type="button" onClick={close}>
          Back
        </button>{" "}
        {chat !== undefined && (
          <button type="button" onClick={() => void opened.load()}>
            Refresh
          </button>
        )}
      </p>
      {opened.problem !== undefined && <Problem text={opened.problem} retry={() => void opened.load()} />}
      {(chat !== undefined || opened.loading) && (
        <TranscriptView lines={shown} away={false} answering={undefined} me={undefined} older={undefined} loading={opened.loading} retry={undefined} />
      )}
    </section>
  );
}

function title(session: OutsideSession): string {
  return `${vendorName(session.agent)} in ${session.folder ?? "an unknown folder"}`;
}

// No status names the agent of an outside session's turns, so they take its vendor's name.
function named(shown: Line[], vendor: string): Line[] {
  return shown.map((line) => (line.kind === "agent" ? { ...line, label: vendor } : line));
}
