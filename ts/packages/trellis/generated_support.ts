/** A runtime codec emitted and composed by generated Trellis packages. */
export type Codec<T> = Readonly<{
  decode(value: unknown): T;
  encode(value: T): unknown;
}>;

/** Serializable fields shared by generated declared errors. */
export type SerializableErrorData = {
  id: string;
  type: string;
  message: string;
  context?: Record<string, unknown>;
  traceId?: string;
} & Record<string, unknown>;

type CodecValue<C> = C extends { decode(value: unknown): infer T } ? T : never;
type OpenEnum<T extends string> =
  | T
  | (string & { readonly __openEnum?: never });
type Timestamp = string & { readonly __timestamp: unique symbol };
type Ulid = string & { readonly __ulid: unique symbol };
type OptionalCodec<T> = Codec<T | undefined> & Readonly<{ optional: true }>;
type ModelField = Codec<unknown> | OptionalCodec<unknown>;
type ModelValue<F extends Readonly<Record<string, ModelField>>> =
  & {
    [K in keyof F as F[K] extends OptionalCodec<unknown> ? never : K]:
      CodecValue<F[K]>;
  }
  & {
    [K in keyof F as F[K] extends OptionalCodec<unknown> ? K : never]?: Exclude<
      CodecValue<F[K]>,
      undefined
    >;
  };

function fail(expected: string): never {
  throw new TypeError(`Expected ${expected}`);
}

function codec<T>(
  decode: (value: unknown) => T,
  encode: (value: T) => unknown = decode,
): Codec<T> {
  return Object.freeze({ decode, encode });
}

function integer(
  name: string,
  minimum: number,
  maximum: number,
): Codec<number> {
  return codec((value) =>
    typeof value === "number" && Number.isInteger(value) && value >= minimum &&
      value <= maximum && !Object.is(value, -0)
      ? value
      : fail(`${name} integer`)
  );
}

function bigintCodec(
  name: string,
  minimum: bigint,
  maximum: bigint,
): Codec<bigint> {
  const pattern = /^(?:0|-?[1-9][0-9]*)$/;
  return codec(
    (value) => {
      if (typeof value !== "string" || !pattern.test(value)) {
        return fail(`${name} decimal string`);
      }
      const decoded = BigInt(value);
      return decoded >= minimum && decoded <= maximum
        ? decoded
        : fail(`${name} decimal string`);
    },
    (value) =>
      typeof value === "bigint" && value >= minimum && value <= maximum
        ? value.toString()
        : fail(`${name} bigint`),
  );
}

const stringCodec = codec<string>((value) =>
  typeof value === "string" ? value : fail("string")
);
const boolCodec = codec<boolean>((value) =>
  typeof value === "boolean" ? value : fail("boolean")
);
const numberCodec = codec<number>((value) =>
  typeof value === "number" && Number.isFinite(value)
    ? value
    : fail("finite number")
);

function decodeBase64(value: unknown): Uint8Array {
  if (
    typeof value !== "string" || value.length % 4 !== 0 ||
    !/^(?:[A-Za-z0-9+/]{4})*(?:[A-Za-z0-9+/]{2}==|[A-Za-z0-9+/]{3}=)?$/.test(
      value,
    )
  ) {
    return fail("padded standard base64 string");
  }
  const binary = atob(value);
  const decoded = Uint8Array.from(
    binary,
    (character) => character.charCodeAt(0),
  );
  return encodeBase64(decoded) === value
    ? decoded
    : fail("padded standard base64 string");
}

function encodeBase64(value: Uint8Array): string {
  if (!(value instanceof Uint8Array)) return fail("Uint8Array");
  let binary = "";
  for (const byte of value) binary += String.fromCharCode(byte);
  return btoa(binary);
}

function timestamp(value: unknown): Timestamp {
  if (typeof value !== "string") return fail("RFC 3339 timestamp");
  const match =
    /^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})(?:\.(\d{1,9}))?(Z|[+-]\d{2}:\d{2})$/i
      .exec(value);
  if (!match) return fail("RFC 3339 timestamp");
  const [, seconds, fraction, offset] = match;
  if (seconds.slice(-2) === "60") {
    return fail("RFC 3339 timestamp without leap second");
  }
  const localSeconds = seconds.replace("t", "T");
  const local = new Date(`${localSeconds}Z`);
  if (
    Number.isNaN(local.getTime()) ||
    local.toISOString().slice(0, 19) !== localSeconds
  ) {
    return fail("RFC 3339 timestamp");
  }
  if (
    offset.toUpperCase() !== "Z" &&
    (+offset.slice(1, 3) > 23 || +offset.slice(4) > 59)
  ) {
    return fail("RFC 3339 timestamp");
  }
  const utc = new Date(`${localSeconds}${offset.toUpperCase()}`);
  if (Number.isNaN(utc.getTime())) return fail("RFC 3339 timestamp");
  const canonical = utc.toISOString().slice(0, 19);
  if (canonical[0] === "+" || canonical[0] === "-") {
    return fail("RFC 3339 timestamp");
  }
  const digits = fraction?.replace(/0+$/, "");
  return `${canonical}${digits ? `.${digits}` : ""}Z` as Timestamp;
}

const ulidPattern = /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/;

/** Browser-safe codec vocabulary consumed by generated TypeScript packages. */
export const codecs = Object.freeze({
  string: stringCodec,
  bool: boolCodec,
  i32: integer("i32", -0x8000_0000, 0x7fff_ffff),
  u32: integer("u32", 0, 0xffff_ffff),
  i64: bigintCodec("i64", -(1n << 63n), (1n << 63n) - 1n),
  u64: bigintCodec("u64", 0n, (1n << 64n) - 1n),
  f64: numberCodec,
  bytes: codec<Uint8Array>(decodeBase64, encodeBase64),
  timestamp: codec<Timestamp>(timestamp),
  ulid: codec<Ulid>((value) =>
    typeof value === "string" && ulidPattern.test(value)
      ? value as Ulid
      : fail("canonical ULID")
  ),
  openEnum<const T extends string>(values: readonly T[]): Codec<OpenEnum<T>> {
    if (values.some((value) => typeof value !== "string")) {
      fail("generated string enum values");
    }
    return codec((value) => {
      if (typeof value !== "string") return fail("string enum value");
      return value as OpenEnum<T>;
    });
  },
  model<const F extends Readonly<Record<string, ModelField>>>(
    fields: F,
  ): Codec<ModelValue<F>> {
    const known = new Set(Object.keys(fields));
    const preserveUnknown = (
      source: Record<string, unknown>,
      target: Record<string, unknown>,
    ): void => {
      for (const [name, value] of Object.entries(source)) {
        if (known.has(name)) continue;
        Object.defineProperty(target, name, {
          value,
          enumerable: true,
          writable: true,
          configurable: true,
        });
      }
    };
    return codec(
      (value) => {
        if (
          value === null || typeof value !== "object" || Array.isArray(value)
        ) {
          return fail("object");
        }
        const source = value as Record<string, unknown>;
        const decoded: Record<string, unknown> = {};
        for (const [name, field] of Object.entries(fields)) {
          if (source[name] === undefined && "optional" in field) continue;
          decoded[name] = field.decode(source[name]);
        }
        preserveUnknown(source, decoded);
        return decoded as ModelValue<F>;
      },
      (value) => {
        if (
          value === null || typeof value !== "object" || Array.isArray(value)
        ) {
          return fail("object");
        }
        const encoded: Record<string, unknown> = {};
        for (const [name, field] of Object.entries(fields)) {
          const fieldValue = (value as Record<string, unknown>)[name];
          if (fieldValue === undefined && "optional" in field) continue;
          encoded[name] = field.encode(fieldValue);
        }
        preserveUnknown(value as Record<string, unknown>, encoded);
        return encoded;
      },
    );
  },
  named<T, Named extends T>(_name: string, value: Codec<T>): Codec<Named> {
    return codec(
      (input) => value.decode(input) as Named,
      (input) => value.encode(input),
    );
  },
  list<T>(item: Codec<T>): Codec<T[]> {
    return codec(
      (value) => Array.isArray(value) ? value.map(item.decode) : fail("array"),
      (value) => Array.isArray(value) ? value.map(item.encode) : fail("array"),
    );
  },
  map<T>(item: Codec<T>): Codec<Record<string, T>> {
    return codec(
      (value) => {
        if (
          value === null || typeof value !== "object" || Array.isArray(value)
        ) {
          return fail("object map");
        }
        return Object.fromEntries(
          Object.entries(value).map((
            [key, entry],
          ) => [key, item.decode(entry)]),
        );
      },
      (value) =>
        Object.fromEntries(
          Object.entries(value).map((
            [key, entry],
          ) => [key, item.encode(entry)]),
        ),
    );
  },
  nullable<T>(value: Codec<T>): Codec<T | null> {
    return codec(
      (input) => input === null ? null : value.decode(input),
      (input) => input === null ? null : value.encode(input),
    );
  },
  optional<T>(value: Codec<T>): OptionalCodec<T> {
    return Object.freeze({
      optional: true as const,
      decode: (input: unknown) =>
        input === undefined ? undefined : value.decode(input),
      encode: (input: T | undefined) =>
        input === undefined ? undefined : value.encode(input),
    });
  },
  ref<T>(get: () => Codec<T>): Codec<T> {
    return codec(
      (value) => get().decode(value),
      (value) => get().encode(value),
    );
  },
  recursive<T>(build: (self: Codec<T>) => Codec<T>): Codec<T> {
    let resolved: Codec<T> | undefined;
    const get = () => {
      resolved ??= build(self);
      return resolved;
    };
    const self = codec<T>(
      (value) => get().decode(value),
      (value) => get().encode(value),
    );
    return self;
  },
});

type ApiDescriptorInput = Readonly<{
  identity: string;
  actions: Readonly<Record<string, Readonly<Record<string, unknown>>>>;
}>;

type PackageEvidenceInput = Readonly<{
  rootPackage: string;
  rootDigest: string;
  packages: readonly Readonly<{
    name: string;
    version: string;
    digest: string;
    source: string;
  }>[];
}>;

type ParticipantDescriptorInput = Readonly<{
  kind: "service" | "device" | "app" | "agent";
  id: string;
  identity: string;
  path: string;
  implements: readonly ApiDescriptorInput[];
  uses: readonly Readonly<Record<string, unknown>>[];
  actionNames: Readonly<Record<string, string>>;
  resources: Readonly<Record<string, Readonly<Record<string, unknown>>>>;
  companion?: Readonly<{
    participant: ParticipantDescriptorInput;
    availability: "required" | "optional";
  }>;
  packageEvidence: PackageEvidenceInput;
}>;

function isRecord(value: unknown): value is Record<string, unknown> {
  return value !== null && typeof value === "object" && !Array.isArray(value);
}

function isCodec(value: unknown): value is Codec<unknown> {
  return isRecord(value) && typeof value.decode === "function" &&
    typeof value.encode === "function";
}

function validateAction(name: string, action: Record<string, unknown>): void {
  if (
    !["rpc", "operation", "event", "live"].includes(String(action.kind)) ||
    action.descriptorName !== name || !name.startsWith(`${action.kind}:`) ||
    name.length === String(action.kind).length + 1
  ) {
    fail("generated action descriptor");
  }
  switch (action.kind) {
    case "rpc":
      if (
        !isCodec(action.input) || !isCodec(action.output) ||
        !Array.isArray(action.errors) ||
        typeof action.download !== "boolean" ||
        (action.pagination !== undefined && action.pagination !== "cursor")
      ) fail("generated RPC descriptor");
      break;
    case "operation":
      if (
        !isCodec(action.input) || !isCodec(action.output) ||
        (action.progress !== undefined && !isCodec(action.progress)) ||
        !Array.isArray(action.errors) ||
        !isRecord(action.signals) ||
        Object.values(action.signals).some((value) => !isCodec(value)) ||
        typeof action.upload !== "boolean"
      ) fail("generated operation descriptor");
      break;
    case "event":
      if (
        !isCodec(action.payload) || !Array.isArray(action.parameters) ||
        action.parameters.some((path) =>
          !Array.isArray(path) || path.some((part) => typeof part !== "string")
        )
      ) fail("generated event descriptor");
      break;
    case "live":
      if (!isCodec(action.input) || !isCodec(action.event)) {
        fail("generated live descriptor");
      }
      break;
  }
}

function validateApiDescriptor(descriptor: ApiDescriptorInput): void {
  if (
    !descriptor || typeof descriptor !== "object" ||
    typeof descriptor.identity !== "string" ||
    descriptor.identity.length === 0 || !isRecord(descriptor.actions)
  ) {
    fail("API descriptor");
  }
  for (const [name, action] of Object.entries(descriptor.actions)) {
    validateAction(name, action);
  }
}

/** Validate and retain generated API evidence without deriving authority from it. */
export function apiDescriptor<const T extends ApiDescriptorInput>(
  descriptor: T,
): T {
  validateApiDescriptor(descriptor);
  for (const action of Object.values(descriptor.actions)) Object.freeze(action);
  Object.freeze(descriptor.actions);
  return Object.freeze(descriptor);
}

function validateResource(resource: Record<string, unknown>): void {
  if (
    !["state", "kv", "store", "job", "consumer"].includes(
      String(resource.kind),
    ) ||
    !["required", "optional"].includes(String(resource.availability))
  ) fail("generated resource descriptor");
  if (resource.kind === "state" || resource.kind === "kv") {
    if (
      !isCodec(resource.codec) || typeof resource.version !== "number" ||
      !Number.isInteger(resource.version) || resource.version <= 0 ||
      !isRecord(resource.migrations)
    ) fail("generated representation descriptor");
    const currentVersion = resource.version;
    if (
      Object.entries(resource.migrations).some(([version, value]) =>
        !/^[1-9][0-9]*$/.test(version) || Number(version) >= currentVersion ||
        !isCodec(value)
      )
    ) fail("generated representation descriptor");
  } else if (
    resource.kind === "job" &&
    (!isCodec(resource.payload) ||
      (resource.result !== undefined && !isCodec(resource.result)) ||
      (resource.update !== undefined && !isCodec(resource.update)))
  ) {
    fail("generated job descriptor");
  }
  if (resource.kind === "job") {
    if (
      resource.deadlineMs !== undefined &&
      (!Number.isInteger(resource.deadlineMs) ||
        Number(resource.deadlineMs) <= 0)
    ) fail("generated job descriptor");
    if (resource.retry !== undefined) {
      if (
        !isRecord(resource.retry) ||
        !Number.isInteger(resource.retry.attempts) ||
        Number(resource.retry.attempts) <= 0 ||
        !Array.isArray(resource.retry.backoffMs) ||
        resource.retry.backoffMs.length !==
          Number(resource.retry.attempts) - 1 ||
        resource.retry.backoffMs.some((value) =>
          !Number.isInteger(value) || Number(value) <= 0
        )
      ) fail("generated job descriptor");
    }
  }
}

function validatePackageEvidence(evidence: PackageEvidenceInput): void {
  if (
    !isRecord(evidence) || typeof evidence.rootPackage !== "string" ||
    evidence.rootPackage.length === 0 ||
    typeof evidence.rootDigest !== "string" ||
    evidence.rootDigest.length === 0 || !Array.isArray(evidence.packages)
  ) fail("package evidence");
  let previous = "";
  let rootFound = false;
  for (const entry of evidence.packages) {
    if (
      !isRecord(entry) || typeof entry.name !== "string" ||
      entry.name.length === 0 || entry.name <= previous ||
      typeof entry.version !== "string" || entry.version.length === 0 ||
      typeof entry.digest !== "string" || entry.digest.length === 0 ||
      typeof entry.source !== "string" || entry.source.length === 0
    ) fail("sorted package evidence");
    previous = entry.name;
    if (entry.name === evidence.rootPackage) {
      if (entry.digest !== evidence.rootDigest) {
        fail("matching root package evidence");
      }
      rootFound = true;
    }
  }
  if (!rootFound) fail("root package evidence");
}

/** Validate and retain generated participant evidence without resolving grants or authority. */
export function participantDescriptor<
  const T extends ParticipantDescriptorInput,
>(
  descriptor: T,
): T {
  if (
    !descriptor || typeof descriptor !== "object" ||
    !["service", "device", "app", "agent"].includes(descriptor.kind) ||
    descriptor.id !== descriptor.identity ||
    typeof descriptor.identity !== "string" ||
    descriptor.identity.length === 0 ||
    typeof descriptor.path !== "string" || descriptor.path.length === 0 ||
    !isRecord(descriptor.packageEvidence) ||
    typeof descriptor.packageEvidence.rootPackage !== "string" ||
    descriptor.identity !==
      `${descriptor.packageEvidence.rootPackage}.${descriptor.path}` ||
    !Array.isArray(descriptor.implements) || !Array.isArray(descriptor.uses) ||
    !isRecord(descriptor.actionNames) ||
    !isRecord(descriptor.resources)
  ) {
    fail("participant descriptor");
  }
  for (const api of descriptor.implements) validateApiDescriptor(api);
  for (const selection of descriptor.uses) {
    if (
      !isRecord(selection) || !isRecord(selection.api) ||
      !Array.isArray(selection.actions) ||
      !Array.isArray(selection.optionalCapabilities)
    ) fail("generated action selection");
    const api = selection.api as unknown as ApiDescriptorInput;
    validateApiDescriptor(api);
    for (const selected of selection.actions) {
      if (
        !isRecord(selected) || typeof selected.descriptorName !== "string" ||
        !(selected.descriptorName in api.actions)
      ) fail("generated action selection");
      const kind = api.actions[selected.descriptorName].kind;
      if (
        (kind === "rpc" && selected.direction !== "call") ||
        (kind === "operation" && selected.direction !== "invoke") ||
        (kind === "event" && selected.direction !== "publish" &&
          selected.direction !== "subscribe") ||
        (kind === "live" && selected.direction !== "subscribe")
      ) fail("generated action direction");
    }
    if (
      selection.optionalCapabilities.some((value) => typeof value !== "string")
    ) {
      fail("generated action selection");
    }
  }
  for (const resource of Object.values(descriptor.resources)) {
    validateResource(resource);
    Object.freeze(resource);
  }
  if (descriptor.companion) {
    if (
      descriptor.kind !== "device" ||
      !isRecord(descriptor.companion.participant) ||
      !["required", "optional"].includes(descriptor.companion.availability) ||
      !["app", "agent"].includes(descriptor.companion.participant.kind) ||
      descriptor.companion.participant.identity.split(".").slice(0, -1).join(
          ".",
        ) !== descriptor.identity ||
      descriptor.companion.participant.companion !== undefined
    ) {
      fail("companion descriptor");
    }
    participantDescriptor(descriptor.companion.participant);
    Object.freeze(descriptor.companion);
  }
  validatePackageEvidence(descriptor.packageEvidence);
  Object.freeze(descriptor.implements);
  Object.freeze(descriptor.uses);
  Object.freeze(descriptor.resources);
  Object.freeze(descriptor.packageEvidence.packages);
  Object.freeze(descriptor.packageEvidence);
  return Object.freeze(descriptor);
}
