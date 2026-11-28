import type { JsonValue } from "@oatscenter/trellis/generated";
import {
  asSchema,
  definitionsOf,
  fieldsOf,
  resolve,
  type Schema,
  type SchemaField,
} from "./schema.ts";

/** What a declared setting carries before anything is stored for it. */
export type SettingDeclaration = {
  readonly name: string;
  readonly title: string;
  readonly description: string;
  readonly schema: unknown;
  readonly required: boolean;
  readonly secret: boolean;
  readonly restart: boolean;
  readonly default?: JsonValue;
};

/**
 * JSON typed into a JSON editor that doesn't parse yet. It stays in the draft
 * so the editor keeps the text, and validation reports it.
 */
export class JsonText {
  readonly text: string;
  constructor(text: string) {
    this.text = text;
  }
}

/**
 * A value being edited: JSON, with `undefined` where nothing is entered yet
 * (an empty number, an unchosen option, an absent optional field) and
 * {@link JsonText} where typed JSON doesn't parse. Validation reports both,
 * so a draft with either is never saved.
 */
export type Draft =
  | null
  | boolean
  | number
  | string
  | JsonText
  | undefined
  | Draft[]
  | { [key: string]: Draft };

/** How a value of one schema node is edited. */
export type Shape =
  | {
      kind: "text";
      format: "text" | "ulid" | "bytes";
      minLength?: number;
      maxLength?: number;
    }
  | { kind: "integer"; min: number; max: number; narrowed: boolean }
  | { kind: "bigint"; min: bigint; max: bigint; narrowed: boolean }
  | { kind: "number"; min?: number; max?: number }
  | { kind: "boolean" }
  | { kind: "choice"; symbols: string[] }
  | { kind: "timestamp" }
  | { kind: "list"; item: Schema; minItems?: number; maxItems?: number }
  | { kind: "map"; value: Schema }
  | { kind: "model"; fields: SchemaField[] }
  | { kind: "nullable"; inner: Schema }
  | { kind: "json"; reason: "any" | "nested" };

/** How deep forms nest before a value is edited as JSON instead. */
const MAX_DEPTH = 6;

const INT_RANGE = {
  int32: [-2147483648, 2147483647],
  uint32: [0, 4294967295],
} as const;

function numberAt(node: Schema, key: string): number | undefined {
  const value = node[key];
  return typeof value === "number" ? value : undefined;
}

/** The root schema of a declared setting and its named definitions. */
export function settingSchema(field: SettingDeclaration): {
  root: Schema;
  defs: Record<string, Schema>;
} {
  const root = asSchema(field.schema) ?? {};
  return { root, defs: definitionsOf(root) };
}

/** How a value of `node` is edited, looking through references and aliases. */
export function shapeOf(
  node: Schema,
  defs: Record<string, Schema>,
  depth = 0,
): Shape {
  if (depth > MAX_DEPTH) return { kind: "json", reason: "nested" };
  let target = resolve(node, defs);
  // A boolean `true` schema (the `json` type) is wrapped in `allOf`.
  const allOf = Array.isArray(target.allOf) ? target.allOf : [];
  if (allOf.length === 1) {
    const only = asSchema(allOf[0]);
    if (!only) return { kind: "json", reason: "any" };
    target = resolve(only, defs);
  }
  if (Array.isArray(target.anyOf)) {
    const members = target.anyOf.flatMap((member) => {
      const schema = asSchema(member);
      return schema ? [resolve(schema, defs)] : [];
    });
    const real = members.filter((member) => member.type !== "null");
    if (real.length === 1 && real.length < members.length && real[0])
      return { kind: "nullable", inner: real[0] };
    return { kind: "json", reason: "any" };
  }
  if (Array.isArray(target["x-trellis-symbols"]))
    return {
      kind: "choice",
      symbols: target["x-trellis-symbols"].map(String),
    };
  switch (target.type) {
    case "boolean":
      return { kind: "boolean" };
    case "number":
      return {
        kind: "number",
        min: numberAt(target, "minimum"),
        max: numberAt(target, "maximum"),
      };
    case "integer": {
      const unsigned = numberAt(target, "minimum") === 0;
      const [low, high] = unsigned ? INT_RANGE.uint32 : INT_RANGE.int32;
      const min = numberAt(target, "minimum") ?? low;
      const max = numberAt(target, "maximum") ?? high;
      return {
        kind: "integer",
        min,
        max,
        narrowed: min !== low || max !== high,
      };
    }
    case "string": {
      const integer = target["x-trellis-integer"];
      if (integer === "int64" || integer === "uint64") {
        const full =
          integer === "int64"
            ? [-(2n ** 63n), 2n ** 63n - 1n]
            : [0n, 2n ** 64n - 1n];
        const bound = (key: string, fallback: bigint) => {
          const text = target[key];
          return typeof text === "string" ? BigInt(text) : fallback;
        };
        const min = bound("x-trellis-minimum", full[0] ?? 0n);
        const max = bound("x-trellis-maximum", full[1] ?? 0n);
        return {
          kind: "bigint",
          min,
          max,
          narrowed: min !== full[0] || max !== full[1],
        };
      }
      if (target.format === "date-time") return { kind: "timestamp" };
      return {
        kind: "text",
        format:
          target.contentEncoding === "base64"
            ? "bytes"
            : target.pattern === "^(?:[A-Z]{1,2})?[0-7][0-9A-HJKMNP-TV-Z]{25}$"
              ? "ulid"
              : "text",
        minLength: numberAt(target, "minLength"),
        maxLength: numberAt(target, "maxLength"),
      };
    }
    case "array": {
      const item = asSchema(target.items);
      if (!item) return { kind: "json", reason: "any" };
      return {
        kind: "list",
        item,
        minItems: numberAt(target, "minItems"),
        maxItems: numberAt(target, "maxItems"),
      };
    }
    case "object": {
      const value = asSchema(target.additionalProperties);
      if (!asSchema(target.properties) && value) return { kind: "map", value };
      return { kind: "model", fields: fieldsOf(target, defs) ?? [] };
    }
  }
  return { kind: "json", reason: "any" };
}

/**
 * A value to start editing `node` with: an empty list or map, a model with
 * its required lists, maps, models and switches filled in, and `false` for a
 * switch. Anything else starts with nothing entered.
 */
export function emptyValue(
  node: Schema,
  defs: Record<string, Schema>,
  depth = 0,
): Draft {
  const shape = shapeOf(node, defs, depth);
  switch (shape.kind) {
    case "boolean":
      return false;
    case "list":
      return [];
    case "map":
      return {};
    case "nullable":
      return emptyValue(shape.inner, defs, depth + 1);
    case "model": {
      const value: { [key: string]: Draft } = {};
      for (const child of shape.fields)
        if (child.required) {
          const start = emptyValue(child.schema, defs, depth + 1);
          if (start !== undefined) value[child.name] = start;
        }
      return value;
    }
    default:
      return undefined;
  }
}

function isRecord(value: Draft): value is { [key: string]: Draft } {
  return (
    typeof value === "object" &&
    value !== null &&
    !Array.isArray(value) &&
    !(value instanceof JsonText)
  );
}

const plural = (count: number, one: string, many = `${one}s`) =>
  `${count} ${count === 1 ? one : many}`;

/** A local time in the form `datetime-local` inputs take, from a UTC timestamp. */
export function toLocalInput(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "";
  const pad = (part: number) => String(part).padStart(2, "0");
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}T${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

/** The UTC timestamp a `datetime-local` input's local time names. */
export function fromLocalInput(text: string): string | undefined {
  if (!text) return undefined;
  const date = new Date(text);
  if (Number.isNaN(date.getTime())) return undefined;
  return date.toISOString().replace(/\.000Z$/, "Z");
}

/**
 * What is wrong with `value` as a value of `node` itself, in plain words; the
 * problems of a list's items or a model's fields are theirs to report.
 * `null` when it fits.
 */
export function problemOf(
  node: Schema,
  defs: Record<string, Schema>,
  value: Draft,
  depth = 0,
): string | null {
  if (value instanceof JsonText) return "This isn't valid JSON.";
  const shape = shapeOf(node, defs, depth);
  if (value === undefined)
    return shape.kind === "choice" ? "Choose a value." : "Enter a value.";
  if (value === null)
    return shape.kind === "nullable" || shape.kind === "json"
      ? null
      : "Enter a value.";
  switch (shape.kind) {
    case "nullable":
      return problemOf(shape.inner, defs, value, depth + 1);
    case "json":
      return null;
    case "boolean":
      return typeof value === "boolean" ? null : "Choose on or off.";
    case "choice":
      if (typeof value !== "string") return "Choose a value.";
      return value === "" ? "Enter a value." : null;
    case "number":
      if (typeof value !== "number" || !Number.isFinite(value))
        return "Enter a number.";
      if (shape.min !== undefined && value < shape.min)
        return `Must be at least ${shape.min}.`;
      if (shape.max !== undefined && value > shape.max)
        return `Must be at most ${shape.max}.`;
      return null;
    case "integer":
      if (typeof value !== "number" || !Number.isFinite(value))
        return "Enter a number.";
      if (!Number.isInteger(value)) return "Use a whole number.";
      if (value < shape.min) return `Must be at least ${shape.min}.`;
      if (value > shape.max) return `Must be at most ${shape.max}.`;
      return null;
    case "bigint": {
      if (typeof value !== "string" || !/^(0|-?[1-9][0-9]*)$/.test(value))
        return "Enter a whole number.";
      const number = BigInt(value);
      if (number < shape.min) return `Must be at least ${shape.min}.`;
      if (number > shape.max) return `Must be at most ${shape.max}.`;
      return null;
    }
    case "timestamp":
      return typeof value === "string" &&
        !Number.isNaN(new Date(value).getTime())
        ? null
        : "Enter a date and time.";
    case "text": {
      if (typeof value !== "string") return "Enter text.";
      const length = [...value].length;
      if (shape.minLength !== undefined && length < shape.minLength)
        return shape.minLength === 1
          ? "Enter a value."
          : `Use at least ${plural(shape.minLength, "character")}.`;
      if (shape.maxLength !== undefined && length > shape.maxLength)
        return `Use at most ${plural(shape.maxLength, "character")}.`;
      if (
        shape.format === "ulid" &&
        !/^(?:[A-Z]{1,2})?[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(value)
      )
        return "Enter an ID as Trellis shows it, in capital letters and digits, such as U01J9Z8X7W6V5T4S3R2Q1P0N9MK.";
      if (
        shape.format === "bytes" &&
        !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
          value,
        )
      )
        return "Enter base64 text, such as aGVsbG8=.";
      return null;
    }
    case "list":
      if (!Array.isArray(value)) return "Enter a list.";
      if (shape.minItems !== undefined && value.length < shape.minItems)
        return `Add at least ${plural(shape.minItems, "item")}.`;
      if (shape.maxItems !== undefined && value.length > shape.maxItems)
        return `Use at most ${plural(shape.maxItems, "item")}.`;
      return null;
    case "map":
      if (!isRecord(value)) return "Enter names and values.";
      return "" in value ? "Each entry needs a name." : null;
    case "model":
      return isRecord(value) ? null : "Enter the fields below.";
  }
}

/** Whether `value`, or anything inside it, has a problem. */
export function hasProblem(
  node: Schema,
  defs: Record<string, Schema>,
  value: Draft,
  depth = 0,
): boolean {
  if (problemOf(node, defs, value, depth) !== null) return true;
  const shape = shapeOf(node, defs, depth);
  switch (shape.kind) {
    case "nullable":
      return value !== null && hasProblem(shape.inner, defs, value, depth + 1);
    case "list":
      return (
        Array.isArray(value) &&
        value.some((item) => hasProblem(shape.item, defs, item, depth + 1))
      );
    case "map":
      return (
        isRecord(value) &&
        Object.values(value).some((item) =>
          hasProblem(shape.value, defs, item, depth + 1),
        )
      );
    case "model":
      return (
        isRecord(value) &&
        shape.fields.some(
          (child) =>
            (child.required || value[child.name] !== undefined) &&
            hasProblem(child.schema, defs, value[child.name], depth + 1),
        )
      );
    default:
      return false;
  }
}

/**
 * The JSON a checked draft stands for: absent optional fields are left out.
 * Throws on a draft with problems, which {@link hasProblem} rules out first.
 */
export function toJson(value: Draft): JsonValue {
  if (value === undefined || value instanceof JsonText)
    throw new Error("The value isn't complete.");
  if (Array.isArray(value)) return value.map(toJson);
  if (isRecord(value)) {
    const result: { [key: string]: JsonValue } = {};
    for (const [key, item] of Object.entries(value))
      if (item !== undefined) result[key] = toJson(item);
    return result;
  }
  return value;
}

/** A value as one short line: text as written, anything else as JSON. */
export function formatValue(value: JsonValue): string {
  return typeof value === "string" ? value : JSON.stringify(value);
}

/** `timeoutSeconds` → `Timeout seconds`, for a model field's label. */
export function fieldLabel(name: string): string {
  const words = name.replace(/([a-z0-9])([A-Z])/g, "$1 $2").toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

/**
 * A secret's value as typed: one whose type is a string (text, an enum, a
 * timestamp), nullable or not, as written; any other as JSON.
 */
export function parseSecret(
  field: SettingDeclaration,
  text: string,
): JsonValue {
  const { root, defs } = settingSchema(field);
  let shape = shapeOf(root, defs);
  if (shape.kind === "nullable") shape = shapeOf(shape.inner, defs, 1);
  if (["text", "choice", "timestamp", "bigint"].includes(shape.kind))
    return text;
  try {
    return JSON.parse(text) as JsonValue;
  } catch {
    throw new Error(`${field.title} must be JSON, such as 9090 or true.`);
  }
}

/** Whether a declared setting's type allows null. */
export function allowsNull(field: SettingDeclaration): boolean {
  const { root, defs } = settingSchema(field);
  const shape = shapeOf(root, defs);
  return shape.kind === "nullable" || shape.kind === "json";
}

/** A setting a user names themselves, for a field no known build declares. */
export type FreeSetting = {
  name: string;
  /** How the value is typed in and stored. */
  format: "text" | "number" | "boolean" | "json";
  secret: boolean;
  value: string;
};

/**
 * The settings and secrets free rows describe. Rows without a name are left
 * out; a bad name, a repeated one, or a value that doesn't fit its format
 * throws with a message for the user.
 */
export function freeSettings(rows: readonly FreeSetting[]): {
  values: Record<string, JsonValue>;
  secrets: Record<string, JsonValue>;
} {
  const values: Record<string, JsonValue> = {};
  const secrets: Record<string, JsonValue> = {};
  for (const row of rows) {
    const name = row.name.trim();
    if (!name) continue;
    if (!/^[a-z][A-Za-z0-9]*$/.test(name))
      throw new Error(
        `“${name}” can't be a setting name. Start with a lowercase letter and use only letters and digits, such as databaseUrl.`,
      );
    if (name in values || name in secrets)
      throw new Error(`${name} is listed twice.`);
    let value: JsonValue = row.value;
    if (row.format === "number") {
      value = Number(row.value);
      if (row.value.trim() === "" || !Number.isFinite(value))
        throw new Error(`${name} must be a number, such as 9090.`);
    } else if (row.format === "boolean") value = row.value === "true";
    else if (row.format === "json") {
      try {
        value = JSON.parse(row.value) as JsonValue;
      } catch {
        throw new Error(
          `${name} must be JSON, such as ["a", "b"] or {"retries": 3}.`,
        );
      }
    }
    (row.secret ? secrets : values)[name] = value;
  }
  return { values, secrets };
}
