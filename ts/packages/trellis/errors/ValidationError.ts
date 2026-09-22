import type { TLocalizedValidationError } from "typebox/error";
import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export type ValidationErrorInput =
  | TLocalizedValidationError
  | { path: string; message: string };

type NormalizedError = { schemaPath: string; message: string };

function normalizeError(e: ValidationErrorInput): NormalizedError {
  if ("schemaPath" in e) {
    return { schemaPath: e.schemaPath, message: e.message ?? "Invalid value" };
  }
  return { schemaPath: e.path, message: e.message };
}

/**
 * Schema for validation issue in ValidationError.
 */
export const ValidationIssueSchema = Type.Object({
  path: Type.String(),
  message: Type.String(),
});
export type ValidationIssue = Static<typeof ValidationIssueSchema>;

/**
 * Schema for ValidationError serialization.
 */
export const ValidationErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("ValidationError"),
  message: Type.String(),
  issues: Type.Array(ValidationIssueSchema),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type ValidationErrorData = Static<typeof ValidationErrorDataSchema>;

/**
 * Error for data validation failures.
 * Includes schema validation and missing required data.
 */
export class ValidationError extends TrellisError<ValidationErrorData> {
  override readonly name = "ValidationError" as const;
  readonly #normalizedErrors: Array<NormalizedError>;

  constructor(
    options: ErrorOptions & {
      errors: Iterable<ValidationErrorInput>;
      context?: Record<string, unknown>;
      id?: string;
    },
  ) {
    const { errors: rawErrors, ...baseOptions } = options;
    const errors = [...rawErrors].map(normalizeError);
    const msg = errors
      .map((e) => `Validation failed. ${e.schemaPath}: ${e.message}.`)
      .join("\n");
    super(msg.length ? msg : "Data validation failed.", baseOptions);
    this.#normalizedErrors = errors;
  }

  /**
   * Serializes error to a plain object.
   * Transforms internal errors array to issues array for serialization.
   *
   * @returns Plain object representation of the error
   */
  override toSerializable(): ValidationErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      issues: this.#normalizedErrors.map((e) => ({
        path: e.schemaPath,
        message: e.message,
      })),
    } as ValidationErrorData;
  }
}
