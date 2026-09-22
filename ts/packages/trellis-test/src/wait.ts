import type { WaitForOptions } from "./types.ts";

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Polls until `fn` returns a truthy value, preserving the last thrown error on timeout. */
export async function waitFor<T>(
  fn: () =>
    | T
    | null
    | undefined
    | false
    | Promise<T | null | undefined | false>,
  opts: WaitForOptions = {},
): Promise<T> {
  const timeoutMs = opts.timeoutMs ?? 5_000;
  const intervalMs = opts.intervalMs ?? 25;
  const startedAt = Date.now();
  let lastError: Error | undefined;

  while (Date.now() - startedAt <= timeoutMs) {
    try {
      const value = await fn();
      if (value) return value;
    } catch (error) {
      lastError = toError(error);
    }
    await delay(intervalMs);
  }

  throw new Error(
    `Timed out after ${timeoutMs}ms waiting for condition${
      lastError ? `: ${lastError.message}` : ""
    }`,
  );
}
