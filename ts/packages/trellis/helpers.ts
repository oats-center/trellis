import { Pointer } from "typebox/value";

import { base64urlEncode, utf8 } from "./auth/utils.ts";

/** Encodes one canonical event parameter value as a NATS subject token. */
export function encodeEventSubjectParameterToken(value: string): string {
  return base64urlEncode(utf8(value));
}

/**
 * Compute the subject template from the message itself
 */
export function template(
  template: string,
  data: Record<string, string | number>,
  opts: { allowWildcards: boolean } = { allowWildcards: false },
): string {
  return template.replace(/\{([^}]+)\}/g, (_, key) => {
    const token = Pointer.Get(data, key);
    if (token === undefined || token === null) {
      if (!opts.allowWildcards) {
        throw new Error("All option templates must have values at runtime.");
      }
      return "*";
    }
    if (typeof token !== "string" && typeof token !== "number") {
      throw new Error("Subject template values must be strings or numbers.");
    }
    if (
      typeof token === "number" &&
      (!Number.isFinite(token) ||
        (Number.isInteger(token) && !Number.isSafeInteger(token)))
    ) {
      throw new Error(
        "Subject template numbers must be finite and safe integers.",
      );
    }

    return encodeEventSubjectParameterToken(
      `${Object.is(token, -0) ? 0 : token}`,
    );
  });
}

const NATS_SUBJECT_TOKEN_FORBIDDEN = /[\u0000\s.*>~]/gu;
export function escapeNats(token: string): string {
  const out = token.replace(
    NATS_SUBJECT_TOKEN_FORBIDDEN,
    (ch) => `~${ch.codePointAt(0)!.toString(16).toUpperCase()}~`,
  );

  // Protect start with $ due to NATS internal use of it
  if (out.length === 0 || out.startsWith("$")) {
    return `_${out}`;
  }

  return out;
}

const NATS_KV_KEY_FORBIDDEN = /[\u0000\s*>~]/gu;
export function escapeKvKey(key: string): string {
  const out = key.replace(
    NATS_KV_KEY_FORBIDDEN,
    (ch) => `~${ch.codePointAt(0)!.toString(16).toUpperCase()}~`,
  );

  if (out.length === 0 || out.startsWith("$")) {
    return `_${out}`;
  }

  return out;
}

export function decodeSubject(token: string): string {
  const out = token.replace(
    /~([0-9A-F]{1,6})~/g,
    (_, hex: string) => String.fromCodePoint(Number.parseInt(hex, 16)),
  );

  if (out.startsWith("_$")) {
    return out.slice(1);
  }

  return out;
}
