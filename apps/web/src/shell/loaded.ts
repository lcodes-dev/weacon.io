import { useEffect, useRef, useState } from "react";

import type { Answer } from "./api.ts";
import { problemText } from "./problems.ts";

/** What a fetched thing shows: its latest good value, and why the latest fetch failed, if it did. */
export type Loaded<T> = { value: T | undefined; problem: string | undefined };

/**
 * Keeps one fetched thing: an older fetch's answer never replaces a newer one or a change made
 * here, and a failed fetch keeps the value shown.
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
    if (this.#shown.problem !== undefined) {
      this.#put({ ...this.#shown, problem: undefined });
    }
    const answer = await fetch();
    if (asked === this.#latest) {
      this.#put(answer.ok ? { value: answer.value, problem: undefined } : { ...this.#shown, problem: problemText(answer.code) });
    }
  }

  /** Changes the value here; it counts as newer than any fetch still on its way. */
  change(update: (value: T | undefined) => T): void {
    this.#latest++;
    this.#put({ ...this.#shown, value: update(this.#shown.value) });
  }

  /** Drops every answer still on its way, as when the page that asked is gone. */
  stop(): void {
    this.#latest++;
  }

  /** Forgets what is shown, for a fetch of something else. */
  reset(): void {
    this.#latest++;
    if (this.#shown.value !== undefined || this.#shown.problem !== undefined) {
      this.#put({ value: undefined, problem: undefined });
    }
  }

  #put(shown: Loaded<T>): void {
    this.#shown = shown;
    this.#show(shown);
  }
}

/**
 * What `fetch` answers, fetched on mount, on each `load`, and afresh whenever `key` changes. A
 * refresh clears nothing but the last problem: loading is when `value` and `problem` are both
 * undefined.
 */
export function useLoaded<T>(
  fetch: () => Promise<Answer<T>>,
  key: unknown,
): Loaded<T> & Pick<Loader<T>, "change"> & { load(): Promise<void> } {
  const [shown, setShown] = useState<Loaded<T>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<T>(setShown));
  const latest = useRef(fetch);
  latest.current = fetch;
  useEffect(() => {
    loader.reset();
    void loader.load(latest.current);
    return () => loader.stop();
  }, [key]);
  return { ...shown, load: () => loader.load(latest.current), change: (update) => loader.change(update) };
}
