import { useEffect, useRef, useState } from "react";

import type { Answer } from "./api.ts";
import { problemText } from "./problems.ts";

/** What a fetched thing shows: its latest good value, and why the latest fetch failed, if it did. */
export type Loaded<T> = { value: T | undefined; problem: string | undefined };

/**
 * Keeps one fetched thing. Each fetch and each local change takes a number, and an answer to an
 * older fetch never replaces newer data; a failed fetch keeps the value shown.
 */
export class Loader<T> {
  #shown: Loaded<T> = { value: undefined, problem: undefined };
  #latest = 0;
  #show: (loaded: Loaded<T>) => void;

  constructor(show: (loaded: Loaded<T>) => void) {
    this.#show = show;
  }

  async load(fetch: () => Promise<Answer<T>>): Promise<void> {
    const asked = ++this.#latest;
    const answer = await fetch();
    if (asked === this.#latest) {
      this.#put(answer.ok ? { value: answer.value, problem: undefined } : { ...this.#shown, problem: problemText(answer.code) });
    }
  }

  /** Changes the value here, as when the page made something the server has already answered for. */
  change(update: (value: T | undefined) => T | undefined): void {
    this.#latest++;
    this.#put({ ...this.#shown, value: update(this.#shown.value) });
  }

  /** Drops every answer still on its way, as when the page that asked is gone. */
  stop(): void {
    this.#latest++;
  }

  #put(shown: Loaded<T>): void {
    this.#shown = shown;
    this.#show(shown);
  }
}

/**
 * What `fetch` answers, fetched on mount, whenever `key` changes, and on each `load`. Nothing is
 * cleared while it refreshes: loading is when `value` and `problem` are both undefined.
 */
export function useLoaded<T>(
  fetch: () => Promise<Answer<T>>,
  key: unknown,
): Loaded<T> & { load(): Promise<void>; change(update: (value: T | undefined) => T | undefined): void } {
  const [shown, setShown] = useState<Loaded<T>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<T>(setShown));
  const latest = useRef(fetch);
  latest.current = fetch;
  useEffect(() => {
    void loader.load(latest.current);
    return () => loader.stop();
  }, [key]);
  return { ...shown, load: () => loader.load(latest.current), change: (update) => loader.change(update) };
}
