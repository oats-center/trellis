import { base as siteBase } from "$app/paths";

import {
  buildConsoleUrl,
  type ConsoleQuery,
  queryString,
  resolveWithBase,
} from "./console/url.ts";

export const base = `${siteBase}/console`;

/**
 * Resolves a Console-local route beneath the unified web application's prefix.
 *
 * Path params are encoded exactly once here. Query values belong in
 * `consoleUrl`/`queryString`, not pasted into `route`, so an opaque identifier
 * cannot be double-encoded or split on a separator inside a value.
 */
export function resolve(
  route: string,
  params: Record<string, string> = {},
): string {
  return resolveWithBase(base, route, params);
}

export type { ConsoleQuery };
export { queryString };

/**
 * Builds one Console-internal URL from a route, path params, and query fields.
 * Each opaque identifier is encoded once; query values use `URLSearchParams`.
 */
export function consoleUrl(
  route: string,
  options: {
    params?: Record<string, string>;
    query?: ConsoleQuery;
  } = {},
): string {
  return buildConsoleUrl(base, route, options);
}
