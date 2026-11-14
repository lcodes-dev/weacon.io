import { useContext, useEffect, useState } from "react";

import type { Later } from "../connection.ts";
import type { MachineId, MachineSummary, PairPending } from "../protocol.ts";
import type { Api } from "../shell/api.ts";
import { useAsk } from "../shell/ask.ts";
import { useLoaded } from "../shell/loaded.ts";
import { WorkspaceSocketContext } from "../shell/workspace-socket.ts";
import { waitForClaim } from "./claim.ts";
import { fingerprint } from "./fingerprint.ts";
import { keep } from "./keep.ts";
import { MachineSettingsView } from "./machine-settings.tsx";
import { osName } from "./os.ts";
import type { Pins } from "./pins.ts";
import { unpair } from "./unpair.ts";

/** The signed-in account's machines: pairing one by its code, the list, and unpairing. */
export function Machines({ api, pins, later }: { api: Api; pins: Pins; later: Later }) {
  const { value, problem, load } = useLoaded(() => api.machines(), api);
  const list = value?.machines;
  const [waiting, setWaiting] = useState<MachineId>();
  const [stalled, setStalled] = useState(false);
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
      setWaiting(undefined);
    }
  }, [list, waiting]);
  return (
    <>
      <h2>Machines</h2>
      {list === undefined && problem === undefined && <p className="muted">Loading</p>}
      {list?.length === 0 && waiting === undefined && (
        <p className="muted">No machines yet. Run weacon pair on one, then enter the code it shows.</p>
      )}
      {list !== undefined && list.length > 0 && (
        <ul aria-label="Machines">
          {list.map((machine) => (
            <Machine key={machine.id} api={api} pins={pins} later={later} machine={machine} unpaired={() => void load()} />
          ))}
        </ul>
      )}
      {problem !== undefined && (
        <p role="alert">
          {problem}{" "}
          <button type="button" onClick={() => void load()}>
            Try again
          </button>
        </p>
      )}
      {waiting !== undefined && <p className="muted">Waiting for the machine to connect.</p>}
      {stalled && <p role="alert">The machine didn't connect. Check that weacon pair is still running there.</p>}
      <Pair
        api={api}
        pins={pins}
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
  pins,
  later,
  machine,
  unpaired,
}: {
  api: Api;
  pins: Pins;
  later: Later;
  machine: MachineSummary;
  unpaired(): void;
}) {
  const [confirming, setConfirming] = useState(false);
  const [open, setOpen] = useState(false);
  // A machine's settings are kept for each workspace, so they show only while one is open.
  const socket = useContext(WorkspaceSocketContext);
  const { busy, problem, ask } = useAsk();
  const paired = new Date(machine.paired_at * 1000).toLocaleDateString();
  return (
    <li>
      <span>{machine.name}</span>
      <span className="muted">
        {osName(machine.os)}, paired {paired}
      </span>
      {confirming ? (
        <span>
          Unpair {machine.name}? It disconnects at once.{" "}
          <button
            type="button"
            disabled={busy}
            onClick={() =>
              void ask(
                () => unpair(api, machine.id),
                () => {
                  // The key goes with the pairing; a failure to forget it leaves a key no one shows.
                  void pins.unpin(machine.id).catch(() => {});
                  unpaired();
                },
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
      {socket?.offers("machine.settings") && (
        <button type="button" aria-label={`${machine.name} settings`} aria-expanded={open} onClick={() => setOpen(!open)}>
          {open ? "Hide settings" : "Settings"}
        </button>
      )}
      {socket?.offers("machine.settings") && open && (
        <MachineSettingsView key={socket.workspace} machine={machine} link={socket} later={later} />
      )}
    </li>
  );
}

type Found = { pending: PairPending; code: string; fingerprint: string };

function Pair({ api, pins, paired }: { api: Api; pins: Pins; paired(id: MachineId): void }) {
  // The code outlives Cancel, so a mistyped look-up can be fixed rather than typed again.
  const [code, setCode] = useState("");
  const [found, setFound] = useState<Found>();
  if (found === undefined) {
    return <LookUp api={api} code={code} setCode={setCode} found={setFound} />;
  }
  return (
    <ConfirmPairing
      api={api}
      pins={pins}
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
  const [unchecked, setUnchecked] = useState(false);
  return (
    <form
      aria-label="Pair a machine"
      onSubmit={(event) => {
        event.preventDefault();
        setUnchecked(false);
        void looking.ask(
          () => api.lookup(code),
          (pending) =>
            void fingerprint(pending.key).then(
              (shown) => found({ pending, code, fingerprint: shown }),
              () => setUnchecked(true),
            ),
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
      {unchecked && <p role="alert">This browser can't check a machine's key on this page. Open Weacon over HTTPS.</p>}
    </form>
  );
}

function ConfirmPairing({
  api,
  pins,
  found,
  paired,
  close,
}: {
  api: Api;
  pins: Pins;
  found: Found;
  paired(id: MachineId): void;
  close(done: boolean): void;
}) {
  const { pending } = found;
  const confirming = useAsk();
  const [saving, setSaving] = useState(false);
  const [local, setLocal] = useState<string>();
  // The server has paired the machine by now: whatever happens here, the list shows it.
  const confirmed = async (machine: MachineSummary) => {
    setSaving(true);
    const problem = await keep(pins, machine, pending);
    setSaving(false);
    paired(machine.id);
    if (problem === undefined) {
      close(true);
    } else {
      setLocal(problem);
    }
  };
  const busy = confirming.busy || saving;
  const alert = confirming.problem ?? local;
  return (
    <section aria-label="Pair a machine">
      <h3>Pair {pending.name}?</h3>
      <dl>
        <dt>Name</dt>
        <dd>{pending.name}</dd>
        <dt>System</dt>
        <dd>{osName(pending.os)}</dd>
        <dt>Fingerprint</dt>
        <dd>{found.fingerprint}</dd>
      </dl>
      <p>Check that the machine's terminal shows the same fingerprint.</p>
      {local === undefined ? (
        <>
          <button
            type="button"
            disabled={busy}
            onClick={() =>
              void confirming.ask(
                () => api.confirm({ pairing: pending.pairing, code: found.code }),
                (machine) => void confirmed(machine),
              )
            }
          >
            Confirm
          </button>{" "}
          <button type="button" disabled={busy} onClick={() => close(false)}>
            Cancel
          </button>
        </>
      ) : (
        <button type="button" onClick={() => close(true)}>
          Close
        </button>
      )}
      {alert !== undefined && <p role="alert">{alert}</p>}
    </section>
  );
}
