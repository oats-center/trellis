import Type, { type Static } from "typebox";
import { TrellisError } from "./TrellisError.ts";

export const SchemaValidationIssueSchema = Type.Object({
  path: Type.String(),
  schemaPath: Type.Optional(Type.String()),
  keyword: Type.String(),
  code: Type.String(),
  message: Type.String(),
  label: Type.Optional(Type.String()),
  note: Type.Optional(Type.String()),
  i18nKey: Type.Optional(Type.String()),
  severity: Type.Optional(Type.Union([
    Type.Literal("error"),
    Type.Literal("warning"),
    Type.Literal("info"),
  ])),
  params: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
});
export type SchemaValidationIssue = Static<typeof SchemaValidationIssueSchema>;

export const SchemaValidationErrorDataSchema = Type.Object({
  id: Type.String(),
  type: Type.Literal("SchemaValidationError"),
  message: Type.String(),
  issues: Type.Array(SchemaValidationIssueSchema),
  context: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
  traceId: Type.Optional(Type.String()),
});
export type SchemaValidationErrorData = Static<
  typeof SchemaValidationErrorDataSchema
>;

export class SchemaValidationError
  extends TrellisError<SchemaValidationErrorData> {
  override readonly name = "SchemaValidationError" as const;
  readonly #issues: SchemaValidationIssue[];

  constructor(
    options: {
      issues: SchemaValidationIssue[];
      context?: Record<string, unknown>;
      cause?: unknown;
      id?: string;
    },
  ) {
    const { issues: rawIssues, ...baseOptions } = options;
    const msg = rawIssues
      .map((issue) => issue.message)
      .join("\n");
    super(msg.length ? msg : "Schema validation failed.", baseOptions);
    this.#issues = rawIssues;
  }

  /** The validation issues that caused this error. */
  get issues(): readonly SchemaValidationIssue[] {
    return this.#issues;
  }

  override toSerializable(): SchemaValidationErrorData {
    return {
      ...this.baseSerializable(),
      type: this.name,
      issues: this.#issues,
    } as SchemaValidationErrorData;
  }
}
