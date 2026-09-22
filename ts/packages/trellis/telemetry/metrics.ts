import { type Histogram, type Meter, metrics } from "@opentelemetry/api";

const TRELLIS_METER_NAME = "@qlever-llc/trellis";
const MAX_ATTRIBUTE_LENGTH = 96;
const LOW_CARDINALITY_PATTERN = /^[A-Za-z0-9_.:-]+$/;
const UUID_PATTERN =
  /(^|[_.:-])[0-9a-f]{8}-[0-9a-f]{4}-[1-5][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}($|[_.:-])/i;
const ULID_PATTERN = /(^|[_.:-])[0-7][0-9A-HJKMNP-TV-Z]{25}($|[_.:-])/i;
const LONG_HEX_SEGMENT_PATTERN = /(^|[_.:-])[0-9a-f]{12,}($|[_.:-])/i;
const TRELLIS_SUBJECT_PREFIX_PATTERN =
  /^(rpc|events|feeds|operations|jobs|state|kv|store|resources|transfer)\.v\d+\./;

const AUTH_REASONS = new Set([
  "invalid_request",
  "missing_session_key",
  "missing_proof",
  "session_not_found",
  "session_expired",
  "invalid_signature",
  "user_not_found",
  "user_already_exists",
  "username_taken",
  "identity_already_exists",
  "identity_not_found",
  "user_inactive",
  "unknown_device",
  "device_deployment_not_found",
  "device_deployment_disabled",
  "device_activation_revoked",
  "unknown_service",
  "service_disabled",
  "iat_out_of_range",
  "invalid_binding_token",
  "session_corrupted",
  "session_already_bound",
  "authtoken_already_used",
  "oauth_session_key_mismatch",
  "reply_subject_mismatch",
  "insufficient_permissions",
  "forbidden",
  "last_admin_required",
  "missing_flow_id",
  "device_activation_flow_not_found",
  "device_activation_flow_expired",
  "device_activation_rejected",
  "device_identity_key_mismatch",
  "invalid_device_qr_mac",
]);

/** Low-cardinality attributes accepted by {@link recordTrellisError}. */
export type TrellisErrorMetricAttributes = {
  /** Stable Trellis surface name, such as `rpc`, `jobs`, or `operations`. */
  surface?: string;
  /** Stable flow direction such as `client`, `server`, `publish`, or `consume`. */
  direction?: string;
  /** Stable operation kind or method name. Do not pass IDs, subjects, or URLs. */
  operation?: string;
  /** Stable lifecycle phase such as `encode`, `send`, `auth`, or `handler`. */
  phase?: string;
  /** Stable local Trellis error type override when the thrown value does not expose one. */
  errorType?: string;
  /** Stable wrapped remote Trellis error type override. */
  remoteErrorType?: string;
  /** Stable auth failure reason, such as `missing_session_key`. */
  authReason?: string;
  /** Static messaging system name, when already known. */
  messagingSystem?: string;
  /** Static messaging operation name, when already known. */
  messagingOperation?: string;
};

/** Duration metric names accepted by {@link recordTrellisDuration}. */
export type TrellisDurationMetricName =
  | "trellis.connect.duration"
  | "trellis.auth.flow.duration"
  | "trellis.auth.approval_resolution.duration"
  | "trellis.auth.callout.duration"
  | "trellis.admin.workflow.duration"
  | "trellis.contract.analysis.duration";

/** Low-cardinality attributes accepted by {@link recordTrellisDuration}. */
export type TrellisDurationMetricAttributes = {
  /** Stable Trellis surface name, such as `rpc`, `jobs`, or `operations`. */
  surface?: string;
  /** Stable operation kind or method name. Do not pass IDs, subjects, or URLs. */
  operation?: string;
  /** Stable lifecycle phase such as `bootstrap`, `encode`, or `total`. */
  phase?: string;
  /** Stable outcome label such as `ok` or `error`. */
  outcome?: string;
  /** Participant kind such as `admin`, `client`, `service`, or `device`. */
  participantKind?: string;
  /** Session kind such as `user`, `service`, or `device`. */
  sessionKind?: string;
  /** Auth flow such as `local`, `oauth`, `device`, or `bootstrap`. */
  authFlow?: string;
  /** Auth approval outcome such as `approved`, `denied`, `required`, or `none`. */
  authApproval?: string;
  /** Whether a deployment authority is present. */
  authorityPresent?: boolean;
  /** Plan classification such as `install`, `update`, `migration`, `noop`, or `unknown`. */
  planClassification?: string;
  /** Contract kind such as `app`, `service`, `device`, `builtin`, or `unknown`. */
  contractKind?: string;
  /** Static messaging system name, when already known. */
  messagingSystem?: string;
  /** Static messaging operation name, when already known. */
  messagingOperation?: string;
};

const DURATION_HISTOGRAM_CACHE = new Map<string, Histogram>();

type SerializableErrorData = {
  type?: unknown;
  reason?: unknown;
  remoteError?: unknown;
  context?: unknown;
};

/** Returns the shared Trellis OpenTelemetry meter. */
export function getTrellisMeter(): Meter {
  return metrics.getMeter(TRELLIS_METER_NAME);
}

/** Records one Trellis error with only stable, low-cardinality attributes. */
export function recordTrellisError(
  error: unknown,
  attributes: TrellisErrorMetricAttributes = {},
): void {
  getTrellisMeter().createCounter("trellis.errors", {
    description: "Trellis errors observed by runtime instrumentation.",
    unit: "{error}",
  }).add(
    1,
    buildTrellisErrorMetricAttributes(
      error,
      attributes,
    ),
  );
}

/**
 * Builds sanitized, low-cardinality attributes for `trellis.errors`.
 *
 * @internal Exported for focused tests; runtime callers should use
 * {@link recordTrellisError}.
 */
export function buildTrellisErrorMetricAttributes(
  error: unknown,
  attributes: TrellisErrorMetricAttributes = {},
): Record<string, string> {
  const serializable = serializableErrorData(error);
  const metricAttributes: Record<string, string> = {
    "exception.type": exceptionType(error),
    "trellis.error.type": trellisErrorType(
      error,
      serializable,
      attributes.errorType,
    ),
  };

  const remoteErrorType = lowCardinalityValue(attributes.remoteErrorType) ??
    remoteSerializableErrorType(serializable);
  if (remoteErrorType) {
    metricAttributes["trellis.remote_error.type"] = remoteErrorType;
  }

  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.surface",
    attributes.surface,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.direction",
    attributes.direction,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.operation",
    attributes.operation,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.phase",
    attributes.phase,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.auth.reason",
    boundedAuthReason(attributes.authReason) ?? authReason(serializable),
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "messaging.system",
    attributes.messagingSystem,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "messaging.operation",
    attributes.messagingOperation,
  );

  return metricAttributes;
}

/**
 * Builds sanitized, low-cardinality attributes for duration histograms.
 *
 * @internal Exported for focused tests; runtime callers should use
 * {@link recordTrellisDuration}.
 */
export function buildTrellisDurationMetricAttributes(
  attributes: TrellisDurationMetricAttributes = {},
): Record<string, string> {
  const metricAttributes: Record<string, string> = {};

  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.surface",
    attributes.surface,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.operation",
    attributes.operation,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.phase",
    attributes.phase,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.outcome",
    attributes.outcome,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.participant.kind",
    attributes.participantKind,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.session.kind",
    attributes.sessionKind,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.auth.flow",
    attributes.authFlow,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.auth.approval",
    attributes.authApproval,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.plan.classification",
    attributes.planClassification,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "trellis.contract.kind",
    attributes.contractKind,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "messaging.system",
    attributes.messagingSystem,
  );
  setLowCardinalityAttribute(
    metricAttributes,
    "messaging.operation",
    attributes.messagingOperation,
  );

  if (attributes.authorityPresent !== undefined) {
    setLowCardinalityAttribute(
      metricAttributes,
      "trellis.authority.present",
      String(attributes.authorityPresent),
    );
  }

  return metricAttributes;
}

/**
 * Records one duration measurement on the given Trellis histogram.
 *
 * Converts `durationMs` to seconds. Negative, NaN, and infinite values are
 * silently ignored.
 *
 * Histogram instruments are cached by metric name so `createHistogram` is
 * called at most once per metric name.
 *
 * @param name - The duration metric name.
 * @param durationMs - The measured duration in milliseconds.
 * @param attributes - Optional low-cardinality attributes. Only values that
 *   pass sanitization are included.
 *
 * @example
 * ```ts
 * recordTrellisDuration("trellis.connect.duration", 42.5, {
 *   phase: "nats_connect",
 *   outcome: "ok",
 *   participantKind: "client",
 * });
 * ```
 */
export function recordTrellisDuration(
  name: TrellisDurationMetricName,
  durationMs: number,
  attributes?: TrellisDurationMetricAttributes,
): void {
  if (
    typeof durationMs !== "number" ||
    durationMs < 0 ||
    !Number.isFinite(durationMs)
  ) {
    return;
  }

  const histogram = getDurationHistogram(name);
  const metricAttributes = buildTrellisDurationMetricAttributes(attributes);

  histogram.record(durationMs / 1000, metricAttributes);
}

function getDurationHistogram(
  name: TrellisDurationMetricName,
): Histogram {
  let histogram = DURATION_HISTOGRAM_CACHE.get(name);
  if (!histogram) {
    histogram = getTrellisMeter().createHistogram(name, {
      description: histogramDescription(name),
      unit: "s",
    });
    DURATION_HISTOGRAM_CACHE.set(name, histogram);
  }
  return histogram;
}

function histogramDescription(name: TrellisDurationMetricName): string {
  switch (name) {
    case "trellis.connect.duration":
      return "Duration of connection lifecycle phases.";
    case "trellis.auth.flow.duration":
      return "Duration of auth flow phases.";
    case "trellis.auth.approval_resolution.duration":
      return "Duration of approval resolution phases.";
    case "trellis.auth.callout.duration":
      return "Duration of NATS auth callout phases.";
    case "trellis.admin.workflow.duration":
      return "Duration of admin workflow phases.";
    case "trellis.contract.analysis.duration":
      return "Duration of contract analysis phases.";
  }
}

function trellisErrorType(
  error: unknown,
  serializable: SerializableErrorData | undefined,
  override: string | undefined,
): string {
  const overrideType = lowCardinalityValue(override);
  if (overrideType) return overrideType;

  const serializableType = lowCardinalityValue(serializable?.type);
  if (serializableType) return serializableType;

  const objectType = objectStringProperty(error, "type");
  if (objectType) return objectType;

  return exceptionType(error);
}

function exceptionType(error: unknown): string {
  if (error instanceof Error) {
    const name = lowCardinalityValue(error.name);
    if (name) return name;
  }

  const objectType = objectStringProperty(error, "type");
  if (objectType) return objectType;

  return "unknown";
}

function serializableErrorData(
  error: unknown,
): SerializableErrorData | undefined {
  const toSerializable = safeProperty(error, "toSerializable");
  if (!isSerializableMethod(toSerializable)) return undefined;

  try {
    const data = toSerializable.call(error);
    return isRecord(data) ? data : undefined;
  } catch {
    return undefined;
  }
}

function remoteSerializableErrorType(
  serializable: SerializableErrorData | undefined,
): string | undefined {
  const remoteError = safeProperty(serializable, "remoteError");
  if (!isRecord(remoteError)) return undefined;
  return objectStringProperty(remoteError, "type");
}

function authReason(
  serializable: SerializableErrorData | undefined,
): string | undefined {
  const type = safeProperty(serializable, "type");
  if (type === "AuthError") {
    return boundedAuthReason(safeProperty(serializable, "reason"));
  }

  if (type === "RemoteError") {
    const remoteError = safeProperty(serializable, "remoteError");
    if (
      isRecord(remoteError) && objectStringProperty(remoteError, "type") ===
        "AuthError"
    ) {
      return boundedAuthReason(safeProperty(remoteError, "reason"));
    }
  }

  return undefined;
}

function boundedAuthReason(value: unknown): string | undefined {
  const sanitized = lowCardinalityValue(value);
  if (!sanitized || !AUTH_REASONS.has(sanitized)) return undefined;
  return sanitized;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isSerializableMethod(
  value: unknown,
): value is (this: unknown) => unknown {
  return typeof value === "function";
}

function safeProperty(value: unknown, key: string): unknown {
  if (!isRecord(value)) return undefined;

  try {
    return value[key];
  } catch {
    return undefined;
  }
}

function objectStringProperty(value: unknown, key: string): string | undefined {
  return lowCardinalityValue(safeProperty(value, key));
}

function setLowCardinalityAttribute(
  attributes: Record<string, string>,
  key: string,
  value: unknown,
): void {
  const sanitized = lowCardinalityValue(value);
  if (sanitized) attributes[key] = sanitized;
}

function lowCardinalityValue(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;

  const trimmed = value.trim();
  if (
    trimmed.length === 0 ||
    trimmed.length > MAX_ATTRIBUTE_LENGTH ||
    !LOW_CARDINALITY_PATTERN.test(trimmed) ||
    UUID_PATTERN.test(trimmed) ||
    ULID_PATTERN.test(trimmed) ||
    LONG_HEX_SEGMENT_PATTERN.test(trimmed) ||
    TRELLIS_SUBJECT_PREFIX_PATTERN.test(trimmed)
  ) {
    return undefined;
  }

  return trimmed;
}
