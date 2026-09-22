export type SchemaDiffField = {
  label: string;
  before: string;
  after: string;
  breaking: boolean;
  beforeSet?: string[];
  afterSet?: string[];
  details?: SchemaDiffField[];
};

type JsonSchema = Record<string, unknown>;

const CONSTRAINT_KEYS = [
  "format",
  "pattern",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "minLength",
  "maxLength",
  "minItems",
  "maxItems",
  "minProperties",
  "maxProperties",
  "multipleOf",
] as const;

export function schemaDiffFields(
  oldSchema: JsonSchema | null,
  newSchema: JsonSchema | null,
): SchemaDiffField[] {
  if (!oldSchema && !newSchema) return [];
  if (!oldSchema || !newSchema) return [];
  const changes = diffObjectShape("", oldSchema, newSchema);
  return changes
    .filter((c) => !(c.before === "—" && c.after === "—"))
    .map((c) => ({
      label: c.label,
      before: c.before,
      after: c.after,
      breaking: c.severity === "breaking",
      ...(c.beforeSet ? { beforeSet: c.beforeSet } : {}),
      ...(c.afterSet ? { afterSet: c.afterSet } : {}),
      ...(c.details && c.details.length > 0
        ? {
          details: c.details.map((d) => ({
            label: d.label,
            before: d.before,
            after: d.after,
            breaking: d.severity === "breaking",
          })),
        }
        : {}),
    }));
}

interface InternalChange {
  label: string;
  before: string;
  after: string;
  severity: "breaking" | "safe" | "review";
  beforeSet?: string[];
  afterSet?: string[];
  details?: InternalChange[];
}

function diffObjectShape(
  pathPrefix: string,
  before: JsonSchema,
  after: JsonSchema,
): InternalChange[] {
  const changes: InternalChange[] = [];

  const beforeType = typeSet(before);
  const afterType = typeSet(after);

  if (!sameSet(beforeType, afterType)) {
    changes.push({
      label: "type",
      before: [...beforeType].join(" | "),
      after: [...afterType].join(" | "),
      severity: isNarrowing(beforeType, afterType) ? "breaking" : "review",
    });
  }

  const beforeRequired = new Set(stringArray(before.required));
  const afterRequired = new Set(stringArray(after.required));

  if (beforeRequired.size > 0 || afterRequired.size > 0) {
    const added = [...afterRequired].filter((f) => !beforeRequired.has(f));
    const removed = [...beforeRequired].filter((f) => !afterRequired.has(f));
    if (added.length > 0 || removed.length > 0) {
      changes.push({
        label: "required",
        before: removed.length > 0 ? removed.join(", ") : "—",
        after: added.length > 0 ? added.join(", ") : "—",
        severity: added.length > 0 ? "breaking" : "safe",
        beforeSet: removed,
        afterSet: added,
      });
    }
  }

  const beforeProps = isRecord(before.properties)
    ? (before.properties as Record<string, JsonSchema>)
    : {};
  const afterProps = isRecord(after.properties)
    ? (after.properties as Record<string, JsonSchema>)
    : {};

  const beforePropNames = Object.keys(beforeProps).sort();
  const afterPropNames = Object.keys(afterProps).sort();
  if (beforePropNames.length > 0 || afterPropNames.length > 0) {
    const addedProps = afterPropNames.filter((f) => !(f in beforeProps));
    const removedProps = beforePropNames.filter((f) => !(f in afterProps));
    if (addedProps.length > 0 || removedProps.length > 0) {
      const shapeDetails: InternalChange[] = [];
      for (const name of addedProps) {
        const prop = afterProps[name];
        shapeDetails.push({
          label: name,
          before: "—",
          after: describeType(prop),
          severity: "safe",
        });
        if (isRecord(prop.properties)) {
          const innerNames = Object.keys(prop.properties).sort();
          if (innerNames.length > 0) {
            const innerList = innerNames.map((n) => {
              const p = (prop.properties as Record<string, JsonSchema>)[n];
              return `${n}: ${describeType(p)}`;
            }).join("; ");
            shapeDetails.push({
              label: `${name} fields`,
              before: "—",
              after: innerList,
              severity: "safe",
            });
          }
        }
      }
      changes.push({
        label: "properties",
        before: removedProps.length > 0 ? removedProps.join(", ") : "—",
        after: addedProps.length > 0 ? addedProps.join(", ") : "—",
        severity: removedProps.length > 0 ? "breaking" : "safe",
        beforeSet: removedProps,
        afterSet: addedProps,
        details: shapeDetails,
      });
    }
  }

  for (const fieldName of Object.keys(beforeProps)) {
    if (!(fieldName in afterProps)) continue;
    const diff = diffField(
      fieldName,
      beforeProps[fieldName],
      afterProps[fieldName],
    );
    if (diff) changes.push(diff);
  }

  for (const key of CONSTRAINT_KEYS) {
    if (!jsonEqual(before[key], after[key])) {
      changes.push({
        label: key,
        before: before[key] !== undefined ? String(before[key]) : "—",
        after: after[key] !== undefined ? String(after[key]) : "—",
        severity: "review",
      });
    }
  }

  const beforeAp = before.additionalProperties;
  const afterAp = after.additionalProperties;
  if (!jsonEqual(beforeAp, afterAp)) {
    changes.push({
      label: "additionalProperties",
      before: beforeAp !== undefined ? String(beforeAp) : "—",
      after: afterAp !== undefined ? String(afterAp) : "—",
      severity: "review",
    });
  }

  for (const key of ["oneOf", "anyOf", "allOf"] as const) {
    const bCount = Array.isArray(before[key]) ? before[key].length : 0;
    const aCount = Array.isArray(after[key]) ? after[key].length : 0;
    if (bCount !== aCount) {
      changes.push({
        label: key,
        before: bCount > 0 ? `${bCount} variants` : "—",
        after: aCount > 0 ? `${aCount} variants` : "—",
        severity: "review",
      });
    }
  }

  const beforeRef = typeof before.$ref === "string" ? before.$ref : null;
  const afterRef = typeof after.$ref === "string" ? after.$ref : null;
  if (beforeRef !== afterRef) {
    changes.push({
      label: "$ref",
      before: beforeRef ?? "—",
      after: afterRef ?? "—",
      severity: "review",
    });
  }

  return changes;
}

function diffField(
  fieldName: string,
  before: JsonSchema,
  after: JsonSchema,
): InternalChange | null {
  const details: InternalChange[] = [];

  const beforeTypes = typeSet(before);
  const afterTypes = typeSet(after);
  if (!sameSet(beforeTypes, afterTypes)) {
    details.push({
      label: "type",
      before: [...beforeTypes].join(" | ") || "—",
      after: [...afterTypes].join(" | ") || "—",
      severity: isNarrowing(beforeTypes, afterTypes) ? "breaking" : "review",
    });
  }

  details.push(...diffEnum(fieldName, before, after));

  for (const key of CONSTRAINT_KEYS) {
    if (!jsonEqual(before[key], after[key])) {
      details.push({
        label: key,
        before: before[key] !== undefined ? String(before[key]) : "—",
        after: after[key] !== undefined ? String(after[key]) : "—",
        severity: "review",
      });
    }
  }

  if (!jsonEqual(before.additionalProperties, after.additionalProperties)) {
    details.push({
      label: "additionalProperties",
      before: before.additionalProperties !== undefined
        ? String(before.additionalProperties)
        : "—",
      after: after.additionalProperties !== undefined
        ? String(after.additionalProperties)
        : "—",
      severity: "review",
    });
  }

  const beforePropNames = isRecord(before.properties)
    ? Object.keys(before.properties).sort()
    : [];
  const afterPropNames = isRecord(after.properties)
    ? Object.keys(after.properties).sort()
    : [];
  const addedKeys = afterPropNames.filter((k) => !beforePropNames.includes(k));
  const removedKeys = beforePropNames.filter((k) =>
    !afterPropNames.includes(k)
  );
  if (addedKeys.length > 0 || removedKeys.length > 0) {
    details.push({
      label: "fields",
      before: removedKeys.length > 0 ? removedKeys.join(", ") : "—",
      after: addedKeys.length > 0 ? addedKeys.join(", ") : "—",
      severity: removedKeys.length > 0 ? "breaking" : "safe",
      beforeSet: removedKeys,
      afterSet: addedKeys,
    });
  }

  if (isRecord(after.properties)) {
    const propList = afterPropNames.map((name) => {
      const p = (after.properties as Record<string, JsonSchema>)[name];
      return `${name}: ${describeType(p)}`;
    });
    if (propList.length > 0) {
      details.push({
        label: "after shape",
        before: "—",
        after: propList.join("; "),
        severity: "safe",
      });
    }
  }

  if (details.length === 0) return null;

  const hasBreaking = details.some((d) => d.severity === "breaking");

  return {
    label: fieldName,
    before: describeType(before),
    after: describeType(after),
    severity: hasBreaking ? "breaking" : "review",
    details,
  };
}

function diffEnum(
  fieldName: string,
  before: JsonSchema,
  after: JsonSchema,
): InternalChange[] {
  const changes: InternalChange[] = [];
  const beforeEnum = Array.isArray(before.enum) ? before.enum : [];
  const afterEnum = Array.isArray(after.enum) ? after.enum : [];
  if (beforeEnum.length === 0 && afterEnum.length === 0) return changes;

  const beforeValues = new Set(beforeEnum.map(stableStringify));
  const afterValues = new Set(afterEnum.map(stableStringify));

  const added = [...afterValues].filter((v) => !beforeValues.has(v));
  const removed = [...beforeValues].filter((v) => !afterValues.has(v));

  if (added.length > 0 || removed.length > 0) {
    changes.push({
      label: "enum",
      before: removed.length > 0 ? removed.join(", ") : "—",
      after: added.length > 0 ? added.join(", ") : "—",
      severity: removed.length > 0 ? "breaking" : "safe",
      beforeSet: removed,
      afterSet: added,
    });
  }

  return changes;
}

function typeSet(schema: JsonSchema): Set<string> {
  const t = schema.type;
  if (Array.isArray(t)) {
    return new Set(t.filter((v): v is string => typeof v === "string"));
  }
  if (typeof t === "string") return new Set([t]);
  return new Set<string>();
}

function describeType(schema: JsonSchema): string {
  const types = typeSet(schema);
  if (types.has("object") && isRecord(schema.properties)) {
    const propCount = Object.keys(schema.properties).length;
    const required = stringArray(schema.required);
    const parts = [`${propCount} field${propCount !== 1 ? "s" : ""}`];
    if (required.length > 0) parts.push(`${required.length} required`);
    return `object (${parts.join(", ")})`;
  }
  if (types.has("array") && isRecord(schema.items)) {
    return `array of ${describeType(schema.items)}`;
  }
  if (types.size > 0) return [...types].join(" | ");
  if (isRecord(schema.properties)) return "object";
  if (Array.isArray(schema.enum)) {
    const vals = schema.enum.map((v) => String(v));
    if (vals.length <= 3) return `enum (${vals.join(", ")})`;
    return `enum (${vals.length} values)`;
  }
  if (schema.$ref) {
    return String(schema.$ref).split("/").pop() ?? String(schema.$ref);
  }
  return "—";
}

function sameSet<T>(a: Set<T>, b: Set<T>): boolean {
  if (a.size !== b.size) return false;
  for (const value of a) {
    if (!b.has(value)) return false;
  }
  return true;
}

function isNarrowing(before: Set<string>, after: Set<string>): boolean {
  for (const value of before) {
    if (!after.has(value)) return true;
  }
  return false;
}

function jsonEqual(a: unknown, b: unknown): boolean {
  return stableStringify(a) === stableStringify(b);
}

function stableStringify(value: unknown): string {
  return JSON.stringify(sortJson(value));
}

function sortJson(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(sortJson);
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value as Record<string, unknown>)
        .sort(([a], [b]) => a.localeCompare(b))
        .map(([key, val]) => [key, sortJson(val)]),
    );
  }
  return value;
}

function stringArray(value: unknown): string[] {
  if (!Array.isArray(value)) return [];
  return value.filter((v): v is string => typeof v === "string");
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
