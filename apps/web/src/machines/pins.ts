import type { MachineId, MachineKey } from "../protocol.ts";
import { isMachineKey } from "./fingerprint.ts";

/** Where pinned keys are kept: IndexedDB in the browser, a map in tests. */
export interface PinStore {
  get(machine: MachineId): Promise<MachineKey | undefined>;
  put(machine: MachineId, key: MachineKey): Promise<void>;
  delete(machine: MachineId): Promise<void>;
}

/** Whether a key a machine shows is the one this browser pinned for it. */
export type KeyCheck = "match" | "mismatch" | "unpinned";

/**
 * The machine keys this browser trusts, each pinned when the person confirmed a pairing here.
 * Only that confirm calls `pin`: nothing else may trust a key without asking the person.
 */
export class Pins {
  #store: PinStore;

  constructor(store: PinStore) {
    this.#store = store;
  }

  pinned(machine: MachineId): Promise<MachineKey | undefined> {
    return this.#store.get(machine);
  }

  /** Refuses a key that isn't one, which no fingerprint could have shown. */
  async pin(machine: MachineId, key: MachineKey): Promise<void> {
    if (!isMachineKey(key)) {
      throw new RangeError("not a machine key");
    }
    await this.#store.put(machine, key);
  }

  unpin(machine: MachineId): Promise<void> {
    return this.#store.delete(machine);
  }

  /** Rejects when the store fails: a caller treats that as a key it can't trust, never as unpinned. */
  async matches(machine: MachineId, key: MachineKey): Promise<KeyCheck> {
    const pinned = await this.#store.get(machine);
    return pinned === undefined ? "unpinned" : pinned === key ? "match" : "mismatch";
  }
}

const DATABASE = "weacon";
const PINS = "pins";

/** Pinned keys in this origin's IndexedDB, which the page's own scripts alone can read. */
export function browserStore(): PinStore {
  let opened: Promise<IDBDatabase> | undefined;
  const forget = () => {
    opened = undefined;
  };
  // A connection that failed or closed is opened again on the next call.
  const database = () =>
    (opened ??= open(forget).catch((error: unknown) => {
      forget();
      throw error;
    }));
  const run = async <T>(mode: IDBTransactionMode, act: (pins: IDBObjectStore) => IDBRequest<T>): Promise<T> => {
    const db = await database();
    return new Promise((resolve, reject) => {
      const transaction = db.transaction(PINS, mode);
      const asked = act(transaction.objectStore(PINS));
      // Settled once committed, so a write that aborts is never reported as kept.
      transaction.oncomplete = () => resolve(asked.result);
      transaction.onabort = () => reject(transaction.error ?? new Error("IndexedDB aborted"));
    });
  };
  return {
    get: async (machine) => {
      const key: unknown = await run("readonly", (pins) => pins.get(machine));
      return typeof key === "string" ? key : undefined;
    },
    put: async (machine, key) => {
      await run("readwrite", (pins) => pins.put(key, machine));
    },
    delete: async (machine) => {
      await run("readwrite", (pins) => pins.delete(machine));
    },
  };
}

function open(closed: () => void): Promise<IDBDatabase> {
  const opening = indexedDB.open(DATABASE, 1);
  opening.onupgradeneeded = (event) => {
    if (event.oldVersion < 1) {
      opening.result.createObjectStore(PINS);
    }
  };
  return new Promise((resolve, reject) => {
    opening.onsuccess = () => {
      const db = opening.result;
      db.onclose = closed;
      // Another tab upgrading the database waits for this one to let go.
      db.onversionchange = () => {
        db.close();
        closed();
      };
      resolve(db);
    };
    opening.onerror = () => reject(opening.error ?? new Error("IndexedDB failed"));
  });
}
