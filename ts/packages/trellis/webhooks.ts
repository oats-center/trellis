/**
 * Webhook endpoints a service declares in its IDL, and general helpers for
 * decide handlers.
 *
 * Trellis serves each endpoint, checks that requests are genuine, and
 * delivers accepted requests as `trellis.webhooks@v1` `Received` events
 * through the endpoint's own durable consumer.
 *
 * @module
 */

import { type AsyncResult, type BaseError } from "@oatscenter/result";

import type {
  WebhookDecideRequest,
  WebhookDecision,
  WebhookDelivery,
  WebhookField,
  WebhookResponse,
} from "./internal_sdk/generated/types/index.js";
import {
  WebhookDecideRequestCodec,
  WebhookDecisionCodec,
} from "./internal_sdk/generated/types/index.js";
import type { UnexpectedError } from "./errors/index.ts";

export type {
  WebhookDecideRequest,
  WebhookDecision,
  WebhookDelivery,
  WebhookField,
  WebhookResponse,
};

/** The arguments a `Received` handler gets. */
export type WebhookReceivedArgs<TClient = unknown> = {
  /** The request exactly as Trellis received it, without credentials. */
  readonly event: WebhookDelivery;
  readonly context: unknown;
  readonly client: TClient;
};

/** Options for an endpoint's `Received` handler. */
export type WebhookReceivedOptions = {
  /** Stops the handler. */
  readonly signal?: AbortSignal;
};

/** A webhook endpoint the service declared. */
export type WebhookEndpoint<TClient = unknown> = {
  /**
   * Handle each accepted request. Delivery is at least once with no ordering
   * guarantee, so the handler must be safe to run twice.
   */
  onReceived(
    handler: (args: WebhookReceivedArgs<TClient>) => unknown | Promise<unknown>,
    options?: WebhookReceivedOptions,
  ): AsyncResult<void, BaseError>;
  /**
   * Decide requests for an endpoint that declares `decide`. The handler runs
   * after the standard verifiers and before duplicate detection, so it can
   * run more than once for the same delivery. A handler that throws counts as
   * failed: a `sync` request gets 503 and an `async` one is tried again.
   */
  decide(
    handler: (
      request: WebhookDecideRequest,
    ) => WebhookDecision | Promise<WebhookDecision>,
    options?: WebhookReceivedOptions,
  ): AsyncResult<void, UnexpectedError>;
};

type WebhookCaller = {
  listenEvent(
    event: string,
    subjectData: Record<string, unknown>,
    handler: (message: unknown, context: unknown) => unknown,
    options?: unknown,
  ): AsyncResult<void, BaseError>;
  serveWebhookDecide(
    subject: string,
    decide: (request: unknown) => Promise<unknown>,
    signal?: AbortSignal,
  ): AsyncResult<void, UnexpectedError>;
};

function subjectToken(value: string): string {
  const bytes = new TextEncoder().encode(value);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary)
    .replaceAll("+", "-")
    .replaceAll("/", "_")
    .replace(/=+$/u, "");
}

/** The runtime event name a webhook endpoint's `Received` event has in a participant. */
export function webhookEventName(endpoint: string): string {
  return `webhook.${endpoint}`;
}

/**
 * @internal Builds the `webhooks` facade of a connected service.
 */
export function createWebhookEndpoints<TClient>(
  resources: Readonly<Record<string, { readonly kind: string }>>,
  caller: WebhookCaller,
  deploymentId: string | undefined,
  client: () => TClient,
): Record<string, WebhookEndpoint<TClient>> {
  const endpoints: Record<string, WebhookEndpoint<TClient>> = {};
  for (const [name, resource] of Object.entries(resources)) {
    if (resource.kind !== "webhook") continue;
    endpoints[name] = {
      onReceived: (handler, options) =>
        caller.listenEvent(
          webhookEventName(name),
          { service: deploymentId ?? "", endpoint: name },
          (message, context) =>
            handler({
              event: message as WebhookDelivery,
              context,
              client: client(),
            }),
          {
            group: name,
            ...(options?.signal ? { signal: options.signal } : {}),
          },
        ),
      decide: (handler, options) =>
        caller.serveWebhookDecide(
          `webhooks.v1.decide.${subjectToken(deploymentId ?? "")}.${subjectToken(name)}`,
          async (request) =>
            WebhookDecisionCodec.encode(
              await handler(WebhookDecideRequestCodec.decode(request)),
            ),
          options?.signal,
        ),
    };
  }
  return endpoints;
}

const encoder = new TextEncoder();

function bytesOf(value: string | Uint8Array): Uint8Array<ArrayBuffer> {
  return typeof value === "string"
    ? encoder.encode(value)
    : new Uint8Array(value);
}

/** Hash algorithms {@link webhookHmac} supports. */
export type WebhookHmacAlgorithm = "sha1" | "sha256" | "sha512";

/**
 * Compute an HMAC of `message` with `key`.
 *
 * @param algorithm - `sha1`, `sha256`, or `sha512`.
 * @param key - The shared secret, as text or bytes.
 * @param message - The exact bytes the sender signed.
 * @returns The raw HMAC bytes.
 */
export async function webhookHmac(
  algorithm: WebhookHmacAlgorithm,
  key: string | Uint8Array,
  message: string | Uint8Array,
): Promise<Uint8Array> {
  const hash = { sha1: "SHA-1", sha256: "SHA-256", sha512: "SHA-512" }[
    algorithm
  ];
  const cryptoKey = await crypto.subtle.importKey(
    "raw",
    bytesOf(key),
    { name: "HMAC", hash },
    false,
    ["sign"],
  );
  return new Uint8Array(
    await crypto.subtle.sign("HMAC", cryptoKey, bytesOf(message)),
  );
}

/**
 * Compare two byte strings in time that depends only on their lengths, so
 * an attacker cannot learn part of a signature from how long a check took.
 */
export function webhookEqual(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  let difference = 0;
  for (let index = 0; index < left.length; index++) {
    difference |= left[index]! ^ right[index]!;
  }
  return difference === 0;
}

/** Signature encodings {@link verifyWebhookHmac} reads. */
export type WebhookSignatureEncoding = "hex" | "base64" | "base64url";

function decodeSignature(
  encoding: WebhookSignatureEncoding,
  signature: string,
): Uint8Array | undefined {
  if (encoding === "hex") {
    if (!/^(?:[0-9a-fA-F]{2})*$/u.test(signature)) return undefined;
    const bytes = new Uint8Array(signature.length / 2);
    for (let index = 0; index < bytes.length; index++) {
      bytes[index] = Number.parseInt(
        signature.slice(index * 2, index * 2 + 2),
        16,
      );
    }
    return bytes;
  }
  try {
    const standard =
      encoding === "base64url"
        ? signature.replaceAll("-", "+").replaceAll("_", "/")
        : signature;
    const binary = atob(standard);
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch {
    return undefined;
  }
}

/**
 * Check a signature the sender sent against an HMAC of `message`, comparing
 * in constant time.
 *
 * @returns Whether the signature matches.
 */
export async function verifyWebhookHmac(options: {
  readonly algorithm: WebhookHmacAlgorithm;
  readonly key: string | Uint8Array;
  readonly message: string | Uint8Array;
  readonly signature: string;
  readonly encoding: WebhookSignatureEncoding;
}): Promise<boolean> {
  const presented = decodeSignature(options.encoding, options.signature);
  if (!presented) return false;
  const expected = await webhookHmac(
    options.algorithm,
    options.key,
    options.message,
  );
  return webhookEqual(presented, expected);
}

/**
 * Whether a timestamp the sender signed is within `toleranceMs` of the time
 * Trellis received the request. Use the delivery's `receivedAt`, not the
 * current time, so a request verified again later still passes.
 */
export function withinWebhookWindow(
  sentAt: Date,
  receivedAt: Date,
  toleranceMs: number,
): boolean {
  return Math.abs(receivedAt.getTime() - sentAt.getTime()) <= toleranceMs;
}

/**
 * Build the response a `sync` decide handler sends the sender. Trellis
 * refuses HTML and script types, cookies, and more than 16 headers or 64 KiB.
 */
export function webhookResponse(options: {
  readonly status?: number;
  readonly contentType?: string;
  readonly body?: string | Uint8Array;
  readonly headers?: Readonly<Record<string, string>>;
}): WebhookResponse {
  const headers: WebhookField[] = Object.entries(options.headers ?? {}).map(
    ([name, value]) => ({ name, value }),
  );
  if (options.contentType) {
    headers.push({ name: "content-type", value: options.contentType });
  }
  return {
    status: options.status ?? 200,
    headers,
    body: options.body === undefined ? new Uint8Array() : bytesOf(options.body),
  };
}
