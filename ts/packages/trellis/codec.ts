import {
  type InferSchemaType,
  type SchemaLike,
  unwrapSchema,
} from "./participant.ts";
import { Result } from "@qlever-llc/result";
import type { StaticDecode, TSchema } from "typebox";
import { EncodeError, ParseError, Value } from "typebox/value";
import {
  SchemaValidationError,
  UnexpectedError,
  ValidationError,
} from "./errors/index.ts";
import type { SchemaValidationIssue } from "./errors/SchemaValidationError.ts";
import type { TLocalizedValidationError } from "typebox/error";
import type {
  TrellisValidationExtension,
  TrellisValidationIssueHint,
} from "./participant_runtime/metadata.ts";

export type JsonValue =
  | string
  | number
  | boolean
  | null
  | JsonValue[]
  | {
    [key: string]: JsonValue;
  };

const X_TRELLIS_VALIDATION = "x-trellis-validation";

function readTrellisValidationExtension(
  schema: unknown,
): TrellisValidationExtension | undefined {
  if (schema === null || schema === undefined || typeof schema !== "object") {
    return undefined;
  }
  const ext = (schema as Record<string, unknown>)[X_TRELLIS_VALIDATION];
  if (ext === null || ext === undefined || typeof ext !== "object") {
    return undefined;
  }
  return ext as TrellisValidationExtension;
}

function resolveSchemaNode(
  root: unknown,
  schemaPath: string,
): unknown | undefined {
  if (!schemaPath.startsWith("#/")) {
    if (schemaPath === "#") return root;
    return undefined;
  }
  const parts = schemaPath.slice(2).split("/").map(decodeURIComponent);
  let current = root;
  for (const part of parts) {
    if (
      current === null || current === undefined || typeof current !== "object"
    ) {
      return undefined;
    }
    current = (current as Record<string, unknown>)[part];
  }
  return current;
}

function resolveRequiredErrorNode(
  root: unknown,
  error: TLocalizedValidationError,
): unknown | undefined {
  const params = error.params as Record<string, unknown> | undefined;
  const missingProperty = (
    params?.missingProperty ??
      (params?.requiredProperties as string[] | undefined)?.[0]
  ) as string | undefined;
  if (!missingProperty) return undefined;

  const objNode = resolveSchemaNode(root, error.schemaPath);
  if (!objNode || typeof objNode !== "object") return undefined;

  const properties = (objNode as Record<string, unknown>).properties;
  if (!properties || typeof properties !== "object") return undefined;

  return (properties as Record<string, unknown>)[missingProperty];
}

const ALLOWED_HINT_KEYWORDS = new Set([
  "required",
  "minItems",
  "maxItems",
  "minLength",
  "maxLength",
  "minimum",
  "maximum",
  "exclusiveMinimum",
  "exclusiveMaximum",
  "pattern",
  "format",
  "enum",
  "const",
]);

function keywordAllowsHint(keyword: string): boolean {
  return ALLOWED_HINT_KEYWORDS.has(keyword);
}

function typeboxIssueToPath(error: TLocalizedValidationError): string {
  if (error.keyword === "required") {
    const params = error.params as Record<string, unknown> | undefined;
    const missingProperty = (
      params?.missingProperty ??
        (params?.requiredProperties as string[] | undefined)?.[0]
    ) as string | undefined;
    if (missingProperty) {
      return error.instancePath
        ? `${error.instancePath}/${missingProperty}`
        : `/${missingProperty}`;
    }
  }
  return error.instancePath || error.schemaPath;
}

function collectValidationIssues(
  root: unknown,
  errors: Iterable<TLocalizedValidationError>,
): {
  annotated: SchemaValidationIssue[];
  unannotated: TLocalizedValidationError[];
} {
  const annotated: SchemaValidationIssue[] = [];
  const unannotated: TLocalizedValidationError[] = [];

  for (const error of [...errors]) {
    const schemaNode = error.keyword === "required"
      ? resolveRequiredErrorNode(root, error)
      : resolveSchemaNode(root, error.schemaPath);

    const extension = schemaNode !== undefined
      ? readTrellisValidationExtension(schemaNode)
      : undefined;

    if (
      extension !== undefined &&
      keywordAllowsHint(error.keyword) &&
      extension.issues?.[error.keyword] !== undefined
    ) {
      const hint = extension.issues[error.keyword]!;
      if (hint.code !== undefined) {
        annotated.push({
          path: typeboxIssueToPath(error),
          schemaPath: error.schemaPath,
          keyword: error.keyword,
          code: hint.code,
          message: hint.message || error.message,
          label: hint.label ?? extension.label,
          note: hint.note ?? extension.note,
          i18nKey: hint.i18nKey,
          severity: hint.severity ?? "error",
          params: error.params as Record<string, unknown>,
        });
        continue;
      }
    }

    unannotated.push(error);
  }

  return { annotated, unannotated };
}

function buildValidationError(
  schema: unknown,
  data: unknown,
  errors: Iterable<TLocalizedValidationError>,
  cause: ParseError | EncodeError,
): SchemaValidationError | ValidationError {
  const { annotated, unannotated } = collectValidationIssues(schema, errors);
  if (annotated.length > 0 && unannotated.length === 0) {
    return new SchemaValidationError({ issues: annotated, cause });
  }
  return new ValidationError({
    errors: unannotated.length > 0
      ? unannotated
      : annotated.map((i) => ({ path: i.path, message: i.message })),
    cause,
  });
}

function parseWithSchema(schema: TSchema, data: JsonValue): unknown {
  return Value.Parse(schema, data);
}

function encodeWithSchema(schema: TSchema, data: unknown): string {
  return JSON.stringify(
    Value.Encode(schema, data),
    (_key, value) => typeof value === "bigint" ? value.toString() : value,
  );
}

export function parse<T extends TSchema>(
  schema: T,
  data: JsonValue,
): Result<
  StaticDecode<T>,
  SchemaValidationError | ValidationError | UnexpectedError
> {
  try {
    return Result.ok(parseWithSchema(schema, data) as StaticDecode<T>);
  } catch (cause) {
    if (cause instanceof ParseError) {
      const errors = Value.Errors(schema, data);
      return Result.err(buildValidationError(schema, data, errors, cause));
    }
    return Result.err(new UnexpectedError({ cause }));
  }
}

export function parseSchema<S extends SchemaLike>(
  schema: S,
  data: JsonValue,
): Result<
  InferSchemaType<S>,
  SchemaValidationError | ValidationError | UnexpectedError
> {
  const raw = unwrapSchema(schema);
  try {
    return Result.ok(
      parseWithSchema(raw as TSchema, data) as InferSchemaType<S>,
    );
  } catch (cause) {
    if (cause instanceof ParseError) {
      const errors = Value.Errors(raw as TSchema, data);
      return Result.err(buildValidationError(raw, data, errors, cause));
    }
    return Result.err(new UnexpectedError({ cause }));
  }
}

/** Parses unknown JSON-compatible data with an arbitrary Trellis schema. */
export function parseUnknownSchema(
  schema: SchemaLike,
  data: JsonValue,
): Result<unknown, SchemaValidationError | ValidationError | UnexpectedError> {
  const raw = unwrapSchema(schema);
  try {
    return Result.ok(parseWithSchema(raw as TSchema, data));
  } catch (cause) {
    if (cause instanceof ParseError) {
      const errors = Value.Errors(raw as TSchema, data);
      return Result.err(buildValidationError(raw, data, errors, cause));
    }
    return Result.err(new UnexpectedError({ cause }));
  }
}

export function encode<T extends TSchema>(
  schema: T,
  data: unknown,
): Result<string, SchemaValidationError | ValidationError | UnexpectedError> {
  try {
    return Result.ok(encodeWithSchema(schema, data));
  } catch (cause) {
    if (cause instanceof EncodeError) {
      const errors = Value.Errors(schema, data);
      return Result.err(buildValidationError(schema, data, errors, cause));
    }
    return Result.err(new UnexpectedError({ cause }));
  }
}

export function encodeSchema(
  schema: SchemaLike,
  data: unknown,
): Result<string, SchemaValidationError | ValidationError | UnexpectedError> {
  const raw = unwrapSchema(schema);
  try {
    return Result.ok(encodeWithSchema(raw as TSchema, data));
  } catch (cause) {
    if (cause instanceof EncodeError) {
      const errors = Value.Errors(raw as TSchema, data);
      return Result.err(buildValidationError(raw, data, errors, cause));
    }
    return Result.err(new UnexpectedError({ cause }));
  }
}
