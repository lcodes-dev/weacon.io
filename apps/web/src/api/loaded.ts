import { useEffect, useRef, useState } from "react";

import type { Answer } from "./api.ts";
import { problemText } from "./problems.ts";

/** What a fetched thing shows: its latest good value, and why the latest fetch failed, if it did. */
export type Loaded<T> = { value: T | undefined; problem: string | undefined };

/**
 * Keeps one fetched thing: one fetch at a time, with loads asked meanwhile run as one more fetch
 * once its answer is shown; an answer never replaces a change made here, and a failed fetch keeps
 * the value shown.
 */
export class Loader<T> {
  #shown: Loaded<T> = { value: undefined, problem: undefined };
  #latest = 0;
  #show: (loaded: Loaded<T>) => void;
  #next: (() => Promise<Answer<T>>) | undefined;
  #running: Promise<void> | undefined;
  #era = 0;

  constructor(show: (loaded: Loaded<T>) => void) {
    this.#show = show;
  }

  /**
   * Settles once no fetch is left to run, so later loads asked meanwhile can keep it waiting; fails
   * if one of those fetches threw.
   */
  load(fetch: () => Promise<Answer<T>>): Promise<void> {
    this.#next = fetch;
    this.#running ??= this.#run();
    return this.#running;
  }

  async #run(): Promise<void> {
    const era = this.#era;
    const thrown: unknown[] = [];
    for (let fetch = this.#next; fetch !== undefined && era === this.#era; fetch = this.#next) {
      this.#next = undefined;
      await this.#fetch(fetch).catch((error: unknown) => thrown.push(error));
    }
    if (era === this.#era) {
      this.#running = undefined;
    }
    if (thrown.length > 0) {
      throw thrown[0];
    }
  }

  async #fetch(fetch: () => Promise<Answer<T>>): Promise<void> {
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

  /** Drops the answer on its way and the load waiting for it, as when the page that asked is gone. */
  stop(): void {
    this.#latest++;
    this.#era++;
    this.#running = undefined;
  }

  /** Forgets what is shown and stops, for a fetch of something else. */
  reset(): void {
    this.stop();
    if (this.#shown.value !== undefined || this.#shown.problem !== undefined) {
      this.#put({ value: undefined, problem: undefined });
    }
  }

  #put(shown: Loaded<T>): void {
    this.#shown = shown;
    this.#show(shown);
  }
}

/** A fetched thing as a page shows it, and how to fetch it again or change it here. */
export type Loading<T> = Loaded<T> & Pick<Loader<T>, "change"> & { loading: boolean; load(): Promise<void> };

/**
 * What `fetch` answers, fetched on mount, on each `load` (one fetch at a time), and afresh whenever
 * `key` changes. A refresh clears nothing but the last problem.
 */
export function useLoaded<T>(fetch: () => Promise<Answer<T>>, key: unknown): Loading<T> {
  const [shown, setShown] = useState<Loaded<T>>({ value: undefined, problem: undefined });
  const [loader] = useState(() => new Loader<T>(setShown));
  const latest = useRef(fetch);
  latest.current = fetch;
  useEffect(() => {
    loader.reset();
    void loader.load(latest.current);
    return () => loader.stop();
  }, [key]);
  return {
    ...shown,
    loading: shown.value === undefined && shown.problem === undefined,
    load: () => loader.load(latest.current),
    change: (update) => loader.change(update),
  };
}
