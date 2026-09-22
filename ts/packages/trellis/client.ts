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
  stream?: string;
  noResponderRetry?: NoResponderRetryOpts;
};
