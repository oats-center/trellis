import { err, isErr, ok, type Result } from "@oatscenter/result";
import { ulid } from "ulid";

import { ValidationError } from "./errors/index.ts";
import { codecs, type Timestamp, type Ulid } from "./generated_support.ts";

export type { Timestamp, Ulid } from "./generated_support.ts";

function parse<T>(
  decode: (value: unknown) => T,
  value: string,
): Result<T, ValidationError> {
  try {
    return ok(decode(value));
  } catch (cause) {
    const message = cause instanceof Error ? cause.message : String(cause);
    return err(new ValidationError({ errors: [{ path: "", message }], cause }));
  }
}

/**
 * The IDL `timestamp` for an instant, as the canonical UTC RFC 3339 string the
 * wire carries.
 *
 * @param value The instant; defaults to now.
 * @returns The timestamp, for example `2026-10-26T02:00:00.5Z`.
 * @throws {TypeError} If `value` is an invalid `Date`.
 */
export function timestamp(value: Date = new Date()): Timestamp {
  if (Number.isNaN(value.getTime())) throw new TypeError("Invalid Date");
  return codecs.timestamp.decode(value.toISOString());
}

/**
 * Parse an RFC 3339 string into the IDL `timestamp` type, normalized to UTC.
 *
 * @param value An RFC 3339 timestamp with a `Z` or numeric offset.
 * @returns The canonical UTC timestamp, or a `ValidationError`.
 */
export function parseTimestamp(
  value: string,
): Result<Timestamp, ValidationError> {
  return parse(codecs.timestamp.decode, value);
}

/**
 * A new IDL `ulid`.
 *
 * @returns A freshly generated canonical ULID.
 */
export function newUlid(): Ulid {
  return codecs.ulid.decode(ulid());
}

/**
 * Parse a string into the IDL `ulid` type.
 *
 * @param value A canonical (uppercase Crockford base32) ULID.
 * @returns The ULID, or a `ValidationError`.
 */
export function parseUlid(value: string): Result<Ulid, ValidationError> {
  return parse(codecs.ulid.decode, value);
}

/** The prefix of each kind of ID Trellis makes, before its ULID. */
export const ID_PREFIXES = Object.freeze({
  user: "U",
  serviceDeployment: "S",
  serviceInstance: "SI",
  deviceDeployment: "D",
  deviceInstance: "DI",
  app: "A",
  role: "R",
  session: "SE",
  job: "J",
  operation: "O",
  instancePrincipal: "P",
  enrollmentSecret: "E",
  serviceClaim: "C",
  serviceUpdate: "SU",
  review: "RV",
  grant: "G",
  roleAssignment: "RA",
  oidcProvider: "OP",
  oidcRoleMapping: "OM",
  linkedSignIn: "L",
  export: "X",
} as const);

/** A kind of ID Trellis makes. */
export type IdKind = keyof typeof ID_PREFIXES;

/**
 * A new ID of `kind`: its prefix, then a new ULID.
 *
 * @param kind The kind of ID, such as `"job"`.
 * @returns The ID, for example `J01JD4Y9Q8B4N4R4E9B4S7G3T2`.
 */
export function newId(kind: IdKind): Ulid {
  return codecs.ulid.decode(`${ID_PREFIXES[kind]}${ulid()}`);
}

/**
 * Parse an ID that came from outside as an ID of one of `kinds`: a bare ULID,
 * as IDs made before prefixes are, or one with the prefix of one of them. The
 * text is kept as it is; what parses is only uppercase letters and digits, so
 * it is one NATS subject token.
 *
 * @param value The ID's text.
 * @param kinds The kinds it may be.
 * @returns The ID, or a `ValidationError` naming what is wrong.
 */
export function parseId(
  value: string,
  ...kinds: IdKind[]
): Result<Ulid, ValidationError> {
  const parsed = parseUlid(value);
  if (isErr(parsed)) return parsed;
  const prefix = value.slice(0, value.length - 26);
  if (prefix === "" || kinds.some((kind) => ID_PREFIXES[kind] === prefix)) {
    return parsed;
  }
  return err(
    new ValidationError({
      errors: [
        {
          path: "",
          message: `${value} is not ${kinds.map((kind) => `a ${kind} ID`).join(" or ")}`,
        },
      ],
    }),
  );
}
