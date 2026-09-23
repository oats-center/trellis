import type { LoggerLike } from "./globals.ts";

type NoResponderRetryOpts = {
  maxAttempts?: number;
  baseDelayMs?: number;
};

/** Transport options shared by caller connections. */
export type ClientOpts = {
  name?: string;
  log?: LoggerLike;
  timeout?: number;
  /** Abandons an in-progress bootstrap and NATS connect when aborted. */
  signal?: AbortSignal;
  stream?: string;
  noResponderRetry?: NoResponderRetryOpts;
};
