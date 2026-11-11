// Writes the protocol's types to the path given, relative to the current folder.

import { writeFileSync } from "node:fs";

import { currentTypes } from "./generate-types.ts";

const [file] = process.argv.slice(2);
if (file === undefined) {
  throw new Error("usage: update-types.ts <file>");
}
writeFileSync(file, currentTypes());
