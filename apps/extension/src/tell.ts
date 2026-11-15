import { ME } from "./build.ts";
import { Failure, HOST, report } from "./core.ts";
import { reported } from "./feedback.ts";

/** A code for `error`, which is a `Failure` when the extension knows what went wrong. */
export function codeOf(error: unknown): string {
  return error instanceof Failure ? error.code : "extension.failed";
}

/** Tells the core about `code`, if it is the extension's own. */
export function tell(code: string): void {
  if (reported(code)) {
    void report(chrome.runtime.connectNative(HOST), ME, code);
  }
}
