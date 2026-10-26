import { err, ok, type Result } from "@oatscenter/result";
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
