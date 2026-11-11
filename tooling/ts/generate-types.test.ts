import assert from "node:assert/strict";
import { test } from "node:test";

import { generate, type Schema } from "./generate-types.ts";

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
    `// Generated from crates/protocol/schema.json by tooling/ts/generate-types.ts. Don't edit it.

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

test("a plain enum is a union of its values", () => {
  const schema: Schema = { title: "Kind", type: "string", enum: ["claude", "codex"] };

  assert.equal(
    generate(schema).split("\n\n").slice(1).join("\n\n"),
    `export type Kind = (
  | "claude"
  | "codex"
);
`,
  );
});

test("anything it doesn't know is refused rather than dropped", () => {
  const cases: [Schema, string][] = [
    [{ title: "A", enum: [{}] }, "unsupported constant: only strings, numbers and booleans"],
    [{ title: "A", enum: [] }, "unsupported enum: it has no values"],
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
