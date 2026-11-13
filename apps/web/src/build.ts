import type { App } from "./protocol.ts";

declare const WEACON_VERSION: string;
declare const WEACON_COMMIT: string;

/** This build, as it introduces itself to the server; `tools/build.ts` defines both values. */
export const ME: App = { name: "weacon-web", version: WEACON_VERSION, commit: WEACON_COMMIT };
