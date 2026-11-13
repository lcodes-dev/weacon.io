import { statusText } from "../connection.ts";
import type { Code } from "../protocol.ts";
import { UNREACHABLE } from "./api.ts";

const PROBLEMS: Record<Code, string> = {
  [UNREACHABLE]: statusText({ state: "ended", code: UNREACHABLE }),
  "sign_in.failed": "That code didn't work. Check it, or ask for a new one.",
  "sign_in.too_many": "Too many tries. Wait a while, then try again.",
};

/** What the page says when a request fails with `code`; codes it doesn't know get a general line. */
export function problemText(code: Code): string {
  return PROBLEMS[code] ?? "Something went wrong. Try again.";
}
