import { env } from "node:process";

/** Reads native service environment configuration when permission is available. */
export function getEnv(key: string): string | undefined {
  try {
    return env[key];
  } catch {
    return undefined;
  }
}
