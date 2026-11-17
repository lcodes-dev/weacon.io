import type { Later } from "../connection.ts";

// A confirmed machine shows once its core claims it, which it does within two minutes.
export const LOOK_AGAIN_MS = 2000;
export const GIVE_UP_MS = 120_000;

/**
 * Reads the list again with `look` until stopped, each time once the last read has answered, and
 * calls `gaveUp` if two minutes pass first. Returns what stops it.
 */
export function waitForClaim(look: () => Promise<unknown>, later: Later, gaveUp: () => void): () => void {
  let stopped = false;
  const again = () =>
    later(() => {
      if (!stopped) {
        // A read that fails is just one look; the next one still comes.
        const next = () => stopped || again();
        void look().then(next, next);
      }
    }, LOOK_AGAIN_MS);
  again();
  later(() => {
    if (!stopped) {
      stopped = true;
      gaveUp();
    }
  }, GIVE_UP_MS);
  return () => {
    stopped = true;
  };
}
