import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

import type { Envelope } from "../src/protocol.ts";
import { TYPES, currentTypes, generate, type Schema } from "./generate-types.ts";

// The type checker rejects these if the generated types stop matching the wire format.
export const SAMPLES: Envelope[] = [
  { id: 1, type: "hello", protocol: 1, app: { name: "weacon-extension", version: "0.1.0", commit: "1a2b3c4" } },
  { id: 1, type: "error", code: "store.full", retryable: false },
  { id: 3, type: "receipt", request: "r-1", state: "failed", code: "run.failed" },
];

test("the committed types are current", () => {
  assert.equal(
    readFileSync(TYPES, "utf8"),
    currentTypes(),
    "src/protocol.ts is out of date: run npm run update-types in apps/extension",
  );
});

test("objects mark optional fields and keep their descriptions", () => {
  const schema: Schema = {
    title: "Thing",
    description: "A thing.\n\nOn three lines; with a */ inside.",
    type: "object",
    properties: {
      name: { type: "string", description: "Its name." },
      count: { type: "integer", minimum: 0 } as Schema,
      tags: { type: "array", items: { $ref: "#/$defs/Tag" } },
      anything: {},
      list: { type: "array" },
      flag: { const: true },
      maybe: { type: ["string", "null"] },
    },
    required: ["name", "tags"],
    $defs: { Tag: { type: "string", maxLength: 8, pattern: "^[a-z]+$" } as Schema, Bag: { type: "object" } },
  };

  assert.equal(
    generate(schema),
    `// Generated from crates/protocol/schema.json by tools/generate-types.ts. Don't edit it.

/**
 * A thing.
 *
 * On three lines; with a *\\/ inside.
 */
export type Thing = {
  /**
   * Its name.
   */
  name: string;
  count?: number;
  tags: Array<Tag>;
  anything?: unknown;
  list?: Array<unknown>;
  flag?: true;
  maybe?: (
    | string
    | null
  );
};

export type Tag = string;

export type Bag = Record<string, unknown>;
`,
  );
});

test("tagged unions join their tag to the referenced type", () => {
  const schema: Schema = {
    title: "Event",
    type: "object",
    properties: { id: { type: "number" } },
    required: ["id"],
    oneOf: [
      {
        type: "object",
        properties: { kind: { type: "string", const: "start" } },
        $ref: "#/$defs/Start",
        required: ["kind"],
      },
      { type: "object", properties: { kind: { const: "stop" }, done: { type: "boolean" } }, required: ["kind"] },
    ],
    $defs: { Start: { type: "object", properties: { at: { const: 2 } } }, Same: { $ref: "#/$defs/Start" } },
  };

  assert.equal(
    generate(schema).split("\n\n").slice(1).join("\n\n"),
    `export type Event = {
  id: number;
} & (
  | {
    kind: "start";
  } & Start
  | {
    kind: "stop";
    done?: boolean;
  }
);

export type Start = {
  at?: 2;
};

export type Same = Start;
`,
  );
});

test("anything it doesn't know is refused rather than dropped", () => {
  const cases: [Schema, string][] = [
    [{ type: "object" }, "the root schema needs a title that is a TypeScript name"],
    [{ title: "not a name" }, "the root schema needs a title that is a TypeScript name"],
    [{ title: "A", $defs: { "b-c": {} } }, "b-c: not a TypeScript name"],
    [{ title: "A", properties: { "b-c": {} } }, "b-c: not a TypeScript name"],
    [{ title: "A", anyOf: [] } as Schema, "unsupported keywords: anyOf"],
    [{ title: "A", type: "tuple" }, "unsupported type: tuple"],
    [{ title: "A", $ref: "other.json#/A" }, "unsupported reference: other.json#/A"],
    [{ title: "A", $ref: "#/$defs/a-b" }, "unsupported reference: #/$defs/a-b"],
    [{ title: "A", const: null }, "unsupported constant: only strings, numbers and booleans"],
  ];

  for (const [schema, message] of cases) {
    assert.throws(() => generate(schema), { message });
  }
});
