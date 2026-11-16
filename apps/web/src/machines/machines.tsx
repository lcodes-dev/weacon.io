import { useEffect, useState } from "react";

import type { Later } from "../connection.ts";
import type { MachineId, MachineSummary, PairPending } from "../protocol.ts";
import type { Api } from "../api/api.ts";
import { useAsk } from "../api/ask.ts";
import { useLoaded } from "../api/loaded.ts";
import { Problem } from "../api/problem.tsx";
import { type WorkspaceSocket, useOffers } from "../shell/workspace-socket.ts";
import { waitForClaim } from "./claim.ts";
import { isOutdated } from "./core.ts";
import { MachineSettingsView } from "./machine-settings.tsx";
import { osName } from "./os.ts";
import { OutsideSessions } from "./outside-sessions.tsx";

/** The signed-in account's machines: pairing one by its code, the list, and unpairing. */
export function Machines({
  api,
  later,
  socket,
  opened,
  team,
}: {
  api: Api;
  later: Later;
  /** The open workspace's socket, for its settings on each machine; none while no workspace is open. */
  socket: WorkspaceSocket | undefined;
  /** A machine whose settings open by themselves, as one just paired does. */
  opened: MachineId | undefined;
  /** Whether other people are in the open workspace, once its members are known. */
  team: boolean | undefined;
}) {
  const { value, problem, loading, load } = useLoaded(() => api.machines(), api);
  const list = value?.machines;
  const [waiting, setWaiting] = useState<MachineId>();
  const [stalled, setStalled] = useState(false);
  const [justPaired, setJustPaired] = useState<MachineId | undefined>(opened);
  useEffect(() => {
    if (waiting === undefined) {
      return;
    }
    return waitForClaim(load, later, () => {
      setWaiting(undefined);
      setStalled(true);
    });
  }, [waiting]);
  useEffect(() => {
    if (list?.some(({ id }) => id === waiting)) {
      setJustPaired(waiting);
      setWaiting(undefined);
    }
  }, [list, waiting]);
  return (
    <>
      <h2>Machines</h2>
      {loading && <p className="muted">Loading</p>}
      {list?.length === 0 && waiting === undefined && (
        <p className="muted">No machines yet. Run weacon pair on one, then enter the code it shows.</p>
      )}
      {list !== undefined && list.length > 0 && (
        <ul aria-label="Machines">
          {list.map((machine) => (
            <Machine key={machine.id} api={api} later={later} socket={socket} team={team} machine={machine} justPaired={machine.id === justPaired} unpaired={() => void load()} />
          ))}
        </ul>
      )}
      {problem !== undefined && <Problem text={problem} retry={() => void load()} />}
      {waiting !== undefined && <p className="muted">Waiting for the machine to connect.</p>}
      {stalled && <p role="alert">The machine didn't connect. Check that weacon pair is still running there.</p>}
      <Pair
        api={api}
        paired={(id) => {
          setStalled(false);
          setWaiting(id);
          void load();
        }}
      />
    </>
  );
}

function Machine({
  api,
  later,
  socket,
  team,
  machine,
  justPaired,
  unpaired,
}: {
  api: Api;
  later: Later;
  socket: WorkspaceSocket | undefined;
  team: boolean | undefined;
  machine: MachineSummary;
  justPaired: boolean;
  unpaired(): void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [open, setOpen] = useState(false);
  // Its settings open by themselves once: a machine just paired, or one opened from New chat,
  // shows its sign-ins.
  useEffect(() => {
    if (justPaired && socket !== undefined) {
      setOpen(true);
    }
  }, [justPaired]);
  // A machine's settings are kept for each workspace, so they show only while one is open.
  const settable = useOffers(socket, "machine.settings");
  // Outside sessions belong to no workspace, but the open one's socket is the way to the machine.
  const readable = useOffers(socket, "sessions.read");
  const [reading, setReading] = useState(false);
  const relays = useOffers(socket, "terminal.relay");
  const { busy, problem, ask } = useAsk();
  const paired = new Date(machine.paired_at * 1000).toLocaleDateString();
  return (
    <li>
      <span>{machine.name}</span>
      <span className="muted">
        {osName(machine.os)}, paired {paired}
        {machine.core !== undefined && `, weacon ${machine.core.version}`}
      </span>
      {isOutdated(machine) && <p role="status">Its weacon is older than this server supports. Install a newer build there.</p>}
      {confirming ? (
        <span>
          Unpair {machine.name}? It disconnects at once.{" "}
          <button
            type="button"
            disabled={busy}
            onClick={() =>
              void ask(
                () => api.unpair(machine.id),
                unpaired,
              )
            }
          >
            Unpair
          </button>{" "}
          <button type="button" disabled={busy} onClick={() => setConfirming(false)}>
            Cancel
          </button>
        </span>
      ) : (
        <button type="button" onClick={() => setConfirming(true)}>
          Unpair
        </button>
      )}
      {problem !== undefined && <span role="alert">{problem}</span>}
      {socket !== undefined && settable && (
        <button type="button" aria-label={`${machine.name} settings`} aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "Hide settings" : "Settings"}
        </button>
      )}
      {socket !== undefined && settable && open && (
        <MachineSettingsView key={socket.workspace} machine={machine} socket={socket} later={later} relays={relays} team={team} />
      )}
      {socket !== undefined && readable && (
        <>
          <button type="button" aria-label={`${machine.name} sessions`} aria-expanded={reading} onClick={() => setReading(!reading)}>
            {reading ? "Hide sessions" : "Sessions"}
          </button>
          {reading && <OutsideSessions machine={machine} socket={socket} later={later} />}
        </>
      )}
    </li>
  );
}

type Found = { pending: PairPending; code: string };

function Pair({ api, paired }: { api: Api; paired(id: MachineId): void }) {
  // The code outlives Cancel, so a mistyped look-up can be fixed rather than typed again.
  const [code, setCode] = useState("");
  const [found, setFound] = useState<Found>();
  if (found === undefined) {
    return <LookUp api={api} code={code} setCode={setCode} found={setFound} />;
  }
  return (
    <ConfirmPairing
      api={api}
      found={found}
      paired={paired}
      close={(done) => {
        if (done) {
          setCode("");
        }
        setFound(undefined);
      }}
    />
  );
}

function LookUp({
  api,
  code,
  setCode,
  found,
}: {
  api: Api;
  code: string;
  setCode(code: string): void;
  found(found: Found): void;
}) {
  const looking = useAsk();
  return (
    <form
      aria-label="Pair a machine"
      onSubmit={(event) => {
        event.preventDefault();
        void looking.ask(
          () => api.lookUpPairing(code),
          (pending) => found({ pending, code }),
        );
      }}
    >
      <h3>Pair a machine</h3>
      <label>
        Code
        <input required value={code} autoComplete="off" onChange={(event) => setCode(event.target.value)} />
      </label>
      <button type="submit" disabled={looking.busy}>
        Look up
      </button>
      {looking.problem !== undefined && <p role="alert">{looking.problem}</p>}
    </form>
  );
}

function ConfirmPairing({
  api,
  found,
  paired,
  close,
}: {
  api: Api;
  found: Found;
  paired(id: MachineId): void;
  close(done: boolean): void;
}) {
  const { pending } = found;
  const confirming = useAsk();
  return (
    <section aria-label="Pair a machine">
      <h3>Pair {pending.name}?</h3>
      <dl>
        <dt>Name</dt>
        <dd>{pending.name}</dd>
        <dt>System</dt>
        <dd>{osName(pending.os)}</dd>
      </dl>
      <button
        type="button"
        disabled={confirming.busy}
        onClick={() =>
          void confirming.ask(
            () => api.confirmPairing({ pairing: pending.pairing, code: found.code }),
            (machine) => {
              paired(machine.id);
              close(true);
            },
          )
        }
      >
        Confirm
      </button>{" "}
      <button type="button" disabled={confirming.busy} onClick={() => close(false)}>
        Cancel
      </button>
      {confirming.problem !== undefined && <p role="alert">{confirming.problem}</p>}
    </section>
  );
}
