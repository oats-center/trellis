import type { JsonValue } from "@oatscenter/trellis/generated";
import { asSchema, definitionsOf, resolve } from "$lib/console/schema.ts";

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

/** The JSON types a declared setting's value may take. */
export function settingTypes(field: SettingDeclaration): unknown[] {
  const root = asSchema(field.schema) ?? {};
  const defs = definitionsOf(root);
  const schema = resolve(root, defs);
  const options = Array.isArray(schema.anyOf)
    ? schema.anyOf.flatMap((option) => {
        const node = asSchema(option);
        return node ? [resolve(node, defs)] : [];
      })
    : [schema];
  return options.flatMap((option) =>
    Array.isArray(option.type) ? option.type : [option.type],
  );
}

/**
 * Text settings, nullable ones included, are typed as text; anything else as
 * JSON.
 */
export function isTextSetting(field: SettingDeclaration): boolean {
  const kinds = settingTypes(field).filter((kind) => kind !== "null");
  return kinds.length === 1 && kinds[0] === "string";
}

/** What a user typed for a declared setting, as the value to store. */
export function parseSetting(
  field: SettingDeclaration,
  text: string,
): JsonValue {
  if (isTextSetting(field)) return text;
  try {
    return JSON.parse(text) as JsonValue;
  } catch {
    throw new Error(`${field.title} must be JSON, such as 9090 or true.`);
  }
}

/** A setting a user names themselves, for a field no known build declares. */
export type FreeSetting = {
  name: string;
  /** Text is stored as written; JSON is parsed. */
  format: "text" | "json";
  secret: boolean;
  value: string;
};

/**
 * The settings and secrets free rows describe. Rows without a name are left
 * out; a bad name, a repeated one, or JSON that doesn't parse throws with a
 * message for the user.
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
    if (row.format === "json") {
      try {
        value = JSON.parse(row.value) as JsonValue;
      } catch {
        throw new Error(`${name} must be JSON, such as 9090, true, or "text".`);
      }
    }
    (row.secret ? secrets : values)[name] = value;
  }
  return { values, secrets };
}
