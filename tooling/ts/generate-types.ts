// Turns the protocol's JSON Schema into TypeScript types. It knows only the keywords schemars
// emits for the protocol, and throws on any other, so a new kind of schema can't be dropped.

import { readFileSync } from "node:fs";

export type Schema = {
  $ref?: string;
  $defs?: Record<string, Schema>;
  type?: string | string[];
  const?: unknown;
  enum?: unknown[];
  properties?: Record<string, Schema>;
  required?: string[];
  items?: Schema;
  oneOf?: Schema[];
  description?: string;
  title?: string;
};

const KNOWN = new Set([
  "$ref",
  "$defs",
  "$schema",
  "type",
  "const",
  "enum",
  "properties",
  "required",
  "items",
  "oneOf",
  "description",
  "title",
  "format",
  "minimum",
  "maxLength",
  "pattern",
]);

const HEADER = "// Generated from crates/protocol/schema.json by tooling/ts/generate-types.ts. Don't edit it.\n";

const SCHEMA = new URL("../../crates/protocol/schema.json", import.meta.url);

/** What an app's `src/protocol.ts` should hold for the committed schema. */
export function currentTypes(): string {
  return generate(JSON.parse(readFileSync(SCHEMA, "utf8")) as Schema);
}

/** The TypeScript source for `root` and its definitions, root first. */
export function generate(root: Schema): string {
  const name = root.title;
  if (name === undefined || !isIdentifier(name)) {
    throw new Error("the root schema needs a title that is a TypeScript name");
  }
  const defs = Object.entries(root.$defs ?? {});
  const types = [[name, root] as const, ...defs].map(([key, schema]) => {
    if (!isIdentifier(key)) {
      throw new Error(`${key}: not a TypeScript name`);
    }
    return `${doc(schema.description, "")}export type ${key} = ${render(schema, "")};\n`;
  });
  return HEADER + types.map((type) => `\n${type}`).join("");
}

function render(schema: Schema, indent: string): string {
  const unknown = Object.keys(schema).filter((key) => !KNOWN.has(key));
  if (unknown.length > 0) {
    throw new Error(`unsupported keywords: ${unknown.join(", ")}`);
  }
  const parts = [base(schema, indent)];
  if (schema.$ref !== undefined) {
    parts.push(reference(schema.$ref));
  }
  if (schema.oneOf !== undefined) {
    parts.push(union(schema.oneOf.map((choice) => render(choice, `${indent}  `)), indent));
  }
  const shown = parts.filter((part) => part !== "unknown");
  return shown.length === 0 ? "unknown" : shown.join(" & ");
}

// The schema's own type, before its `$ref` and `oneOf` are joined to it; `unknown` if none.
function base(schema: Schema, indent: string): string {
  if (schema.const !== undefined) {
    return literal(schema.const);
  }
  if (schema.enum !== undefined) {
    if (schema.enum.length === 0) {
      throw new Error("unsupported enum: it has no values");
    }
    return union(schema.enum.map(literal), indent);
  }
  if (Array.isArray(schema.type)) {
    return union(
      schema.type.map((type) => base({ ...schema, type }, indent)),
      indent,
    );
  }
  switch (schema.type) {
    case undefined:
      return schema.properties === undefined ? "unknown" : object(schema, indent);
    case "string":
    case "boolean":
    case "null":
      return schema.type;
    case "integer":
    case "number":
      return "number";
    case "array":
      return `Array<${schema.items === undefined ? "unknown" : render(schema.items, indent)}>`;
    case "object":
      return object(schema, indent);
    default:
      throw new Error(`unsupported type: ${schema.type}`);
  }
}

function object(schema: Schema, indent: string): string {
  const properties = Object.entries(schema.properties ?? {});
  if (properties.length === 0) {
    // A bare object is only the frame for a `$ref` or `oneOf` beside it.
    return schema.$ref !== undefined || schema.oneOf !== undefined ? "unknown" : "Record<string, unknown>";
  }
  const required = new Set(schema.required ?? []);
  const inner = `${indent}  `;
  const lines = properties.map(([key, property]) => {
    if (!isIdentifier(key)) {
      throw new Error(`${key}: not a TypeScript name`);
    }
    const optional = required.has(key) ? "" : "?";
    return `${doc(property.description, inner)}${inner}${key}${optional}: ${render(property, inner)};\n`;
  });
  return `{\n${lines.join("")}${indent}}`;
}

function union(members: string[], indent: string): string {
  return `(\n${members.map((member) => `${indent}  | ${member}\n`).join("")}${indent})`;
}

function reference(ref: string): string {
  const name = ref.match(/^#\/\$defs\/(.+)$/)?.[1];
  if (name === undefined || !isIdentifier(name)) {
    throw new Error(`unsupported reference: ${ref}`);
  }
  return name;
}

function literal(value: unknown): string {
  if (typeof value === "string" || typeof value === "number" || typeof value === "boolean") {
    return JSON.stringify(value);
  }
  throw new Error("unsupported constant: only strings, numbers and booleans");
}

function doc(description: string | undefined, indent: string): string {
  if (description === undefined) {
    return "";
  }
  const lines = description.replaceAll("*/", "*\\/").split("\n");
  return `${indent}/**\n${lines.map((line) => `${indent} * ${line}`.trimEnd() + "\n").join("")}${indent} */\n`;
}

function isIdentifier(name: string): boolean {
  return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(name);
}
