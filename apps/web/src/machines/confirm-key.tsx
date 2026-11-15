import { useEffect, useState } from "react";

import type { MachineSummary } from "../protocol.ts";
import { type Checked, checkKey, shownStep } from "./check-key.ts";
import type { Pins } from "./pins.ts";

/**
 * Lets the person trust `machine`'s key in this browser, as pairing in it would have, by checking
 * its fingerprint against what `weacon doctor` prints on the machine.
 */
export function ConfirmKey({ pins, machine }: { pins: Pins; machine: MachineSummary }) {
  const [checked, setChecked] = useState<Checked>();
  const [problem, setProblem] = useState<string>();
  // Checked once per machine: a key that changes after is never shown as one to confirm.
  useEffect(() => {
    let live = true;
    void checkKey(pins, machine.id, machine.key).then((found) => live && setChecked(found));
    return () => {
      live = false;
    };
  }, [pins, machine.id]);
  const { name } = machine;
  const step = shownStep(checked, machine.key);
  if (step.kind === "none") {
    return null;
  }
  if (step.kind === "failed") {
    return <p className="muted">This browser couldn't check {name}'s key, so its terminal won't open here.</p>;
  }
  if (step.kind === "mismatch") {
    return <p role="alert">This browser trusts another key for {name}. If you didn't pair it again, unpair it.</p>;
  }
  if (step.kind === "changed") {
    return <p role="alert">{name}'s key changed after this page checked it. If you don't know why, unpair it.</p>;
  }
  const { key, shown } = step;
  const confirm = () =>
    void pins.pin(machine.id, key).then(
      () => setChecked({ kind: "match", key }),
      () => setProblem("This browser couldn't keep the key. Reload the page and check it again."),
    );
  return (
    <section aria-label={`Confirm ${name}'s key`}>
      <p>
        This browser hasn't confirmed {name}'s key, so its terminal won't open here. Run weacon doctor on {name} and check that it prints this key:{" "}
        <code>{shown}</code>
      </p>
      <button type="button" onClick={confirm}>
        It matches
      </button>
      {problem !== undefined && <p role="alert">{problem}</p>}
    </section>
  );
}
