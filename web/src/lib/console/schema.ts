/**
 * Reading the JSON Schema documents an API's actions carry: a root `$ref`
 * into `$defs`, one definition per named model, enum or type.
 */
export type Schema = { readonly [key: string]: unknown };

/** One field of a model, in authored order of importance: required first. */
export type SchemaField = {
  name: string;
  required: boolean;
  schema: Schema;
  description?: string;
};

function isSchema(value: unknown): value is Schema {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** The named definitions a root schema carries. */
export function definitionsOf(root: unknown): Record<string, Schema> {
  const defs = isSchema(root) ? root.$defs : undefined;
  const result: Record<string, Schema> = {};
  if (isSchema(defs))
    for (const [key, value] of Object.entries(defs))
      if (isSchema(value)) result[key] = value;
  return result;
}

/** `#/$defs/trellis.AuthUser` → `AuthUser`. */
export function refName(ref: string): string {
  const key = ref.replace(/^#\/\$defs\//, "");
  return key.slice(key.lastIndexOf(".") + 1);
}

function refKey(node: Schema): string | null {
  return typeof node.$ref === "string"
    ? node.$ref.replace(/^#\/\$defs\//, "")
    : null;
}

/** The definition a `$ref` points at, or the node itself. */
export function resolve(node: Schema, defs: Record<string, Schema>): Schema {
  const seen = new Set<string>();
  let current = node;
  for (
    let key = refKey(current);
    key && !seen.has(key);
    key = refKey(current)
  ) {
    seen.add(key);
    const next = defs[key];
    if (!next) break;
    current = next;
  }
  return current;
}

function count(node: Schema, min: string, max: string, unit: string): string {
  const low = typeof node[min] === "number" ? node[min] : undefined;
  const high = typeof node[max] === "number" ? node[max] : undefined;
  if (low === undefined && high === undefined) return "";
  if (low !== undefined && high !== undefined)
    return ` (${low}–${high} ${unit})`;
  return low !== undefined
    ? ` (at least ${low} ${unit})`
    : ` (up to ${high} ${unit})`;
}

/** A short, readable type: `list of AuthUser`, `timestamp`, `AuthUser or null`. */
export function typeLabel(node: Schema, defs: Record<string, Schema>): string {
  if (typeof node.$ref === "string") return refName(node.$ref);
  const anyOf = Array.isArray(node.anyOf) ? node.anyOf.filter(isSchema) : [];
  if (anyOf.length) {
    const types = anyOf.map((member) =>
      member.type === "null" ? "null" : typeLabel(member, defs),
    );
    return types.join(" or ");
  }
  if (Array.isArray(node["x-trellis-symbols"]))
    return `one of ${node["x-trellis-symbols"].map(String).join(", ")}`;
  if (typeof node["x-trellis-integer"] === "string")
    return node["x-trellis-integer"];
  switch (node.type) {
    case "array":
      return `list of ${isSchema(node.items) ? typeLabel(node.items, defs) : "anything"}${count(node, "minItems", "maxItems", "items")}`;
    case "object":
      if (!isSchema(node.properties) && isSchema(node.additionalProperties))
        return `map of ${typeLabel(node.additionalProperties, defs)}`;
      return "object";
    case "string":
      if (node.format === "date-time") return "timestamp";
      if (node.contentEncoding === "base64") return "bytes";
      if (node.pattern === "^[0-7][0-9A-HJKMNP-TV-Z]{25}$") return "ULID";
      return `string${count(node, "minLength", "maxLength", "characters")}`;
    case "integer":
      return node.minimum === 0 ? "uint32" : "int32";
    case "number":
      return "number";
    case "boolean":
      return "bool";
    case "null":
      return "null";
  }
  return "any JSON";
}

/**
 * The fields a value of this type has, looking through references, aliases,
 * lists and nullability; `null` when it is not a model.
 */
export function fieldsOf(
  node: Schema,
  defs: Record<string, Schema>,
): SchemaField[] | null {
  const target = resolve(node, defs);
  const anyOf = Array.isArray(target.anyOf)
    ? target.anyOf.filter(isSchema).filter((member) => member.type !== "null")
    : [];
  if (anyOf.length === 1 && anyOf[0]) return fieldsOf(anyOf[0], defs);
  if (target.type === "array" && isSchema(target.items))
    return fieldsOf(target.items, defs);
  if (!isSchema(target.properties)) return null;
  const required = new Set(
    Array.isArray(target.required) ? target.required.map(String) : [],
  );
  return Object.entries(target.properties)
    .filter((entry): entry is [string, Schema] => isSchema(entry[1]))
    .map(([name, schema]) => ({
      name,
      required: required.has(name),
      schema,
      description:
        typeof schema.description === "string" ? schema.description : undefined,
    }))
    .sort((a, b) => Number(b.required) - Number(a.required));
}

/** The authored description of a type, read through its reference. */
export function descriptionOf(
  node: Schema,
  defs: Record<string, Schema>,
): string | undefined {
  if (typeof node.description === "string") return node.description;
  const target = resolve(node, defs);
  return typeof target.description === "string"
    ? target.description
    : undefined;
}

/** Narrows a contract `json` value to a schema node. */
export function asSchema(value: unknown): Schema | null {
  return isSchema(value) ? value : null;
}
