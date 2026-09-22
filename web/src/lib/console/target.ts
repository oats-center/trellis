/**
 * Exact-target resolution for console action routes.
 *
 * A route reached with an explicit target ID must resolve exactly that record:
 * through the contract's `Get` when one exists, otherwise through a complete
 * traversal of a correctly filtered list. Absence is never satisfied by another
 * row, and a lookup that failed partway is reported as incomplete rather than
 * as absence.
 *
 * The resolution distinguishes states a page must render differently:
 *
 * - `not-requested` — the route has no target; the page shows a selector.
 * - `loading` — a lookup is in flight.
 * - `ready` — the exact record, with its server-returned state.
 * - `not-found` — the target is absent from a complete lookup.
 * - `forbidden` — the caller may not read or use this target.
 * - `error` — the lookup failed; the target's existence is unknown.
 * - `incomplete` — a later page failed, so absence is not established.
 */

import type { BaseError, Result } from "@qlever-llc/result";
import { isErr } from "@qlever-llc/result";

import { projectConsoleError } from "./display_value.ts";
import { catalogPage, type CursorPage, type PageRequest } from "./paging.ts";
import { RequestScope, type RequestToken } from "./request_scope.ts";

/** Why a target could not be resolved. */
export type TargetFailureKind = "not-found" | "forbidden" | "error";

/** One target resolution outcome. */
export type TargetResolution<T> =
  | { readonly kind: "not-requested" }
  | { readonly kind: "loading" }
  | { readonly kind: "ready"; readonly item: T }
  | { readonly kind: "not-found" }
  | { readonly kind: "forbidden"; readonly error: unknown }
  | { readonly kind: "error"; readonly error: unknown }
  | { readonly kind: "incomplete"; readonly error: unknown };

/** Classifies a read failure into a target-resolution state. */
export function classifyTargetFailure(error: unknown): TargetFailureKind {
  const code = projectConsoleError(error).code;
  if (code === "not_found") return "not-found";
  if (code === "not_authorized") return "forbidden";
  return "error";
}

/** Exact-`Get` lookup for contracts that supply one. */
export type TargetGet<T> = (id: string) => Promise<Result<T, BaseError>>;

/** Page source for contracts without an exact `Get`. */
export type TargetListPage<T> = (
  page: PageRequest,
) => Promise<Result<CursorPage<T>, BaseError>>;

export type ResolveTargetArgs<T> = {
  readonly requestedId: string;
  /** Exact `Get`; preferred when the contract supplies one. */
  readonly get?: TargetGet<T>;
  /** Complete-traversal source used when no exact `Get` exists. */
  readonly listPage?: TargetListPage<T>;
  /** Canonical ID of a listed record. */
  readonly idOf: (item: T) => string;
  readonly timeoutMs?: number;
};

function toCursorPage<T>(
  items: readonly T[],
  nextCursor: string | null | undefined,
): CursorPage<T> {
  return nextCursor == null || nextCursor === ""
    ? { items }
    : { items, cursor: nextCursor };
}

/**
 * Resolves one explicitly requested target.
 *
 * Uses the exact `Get` when present. Otherwise traverses every page, cycling
 * protection included, and reports `incomplete` when a later page failed so the
 * caller cannot present a failed lookup as absence.
 */
export async function resolveRequestedTarget<T>(
  args: ResolveTargetArgs<T>,
): Promise<TargetResolution<T>> {
  const requestedId = args.requestedId;
  if (requestedId === "") return { kind: "not-requested" };

  if (args.get !== undefined) {
    const get = args.get;
    let taken: T | Result<never, BaseError>;
    try {
      taken = (await get(requestedId)).take();
    } catch (error) {
      const failure = classifyTargetFailure(error);
      return failure === "not-found"
        ? { kind: "not-found" }
        : failure === "forbidden"
        ? { kind: "forbidden", error }
        : { kind: "error", error };
    }
    if (isErr(taken)) {
      const failure = classifyTargetFailure(taken.error);
      return failure === "not-found"
        ? { kind: "not-found" }
        : failure === "forbidden"
        ? { kind: "forbidden", error: taken.error }
        : { kind: "error", error: taken.error };
    }
    // The Get is authoritative for identity: a response for a different ID is a
    // server contract violation, not a usable target.
    const resolved: T = taken;
    if (args.idOf(resolved) !== requestedId) {
      return {
        kind: "error",
        error: new Error("target lookup returned a different record"),
      };
    }
    return { kind: "ready", item: resolved };
  }

  const listPage = args.listPage;
  if (listPage === undefined) {
    return {
      kind: "error",
      error: new Error("target resolution needs an exact Get or a list page"),
    };
  }
  let cursor: string | undefined;
  const seen = new Set<string>();
  for (;;) {
    let taken: CursorPage<T> | Result<never, BaseError>;
    try {
      taken = (await listPage(catalogPage(cursor))).take();
    } catch (error) {
      return { kind: "incomplete", error };
    }
    if (isErr(taken)) {
      const failure = classifyTargetFailure(taken.error);
      // A denied or failed traversal is not absence.
      if (failure === "forbidden") {
        return { kind: "forbidden", error: taken.error };
      }
      if (failure === "not-found") {
        return { kind: "not-found" };
      }
      return cursor === undefined
        ? { kind: "error", error: taken.error }
        : { kind: "incomplete", error: taken.error };
    }
    const page: CursorPage<T> = toCursorPage(taken.items, taken.cursor);
    const item = page.items.find((entry) => args.idOf(entry) === requestedId);
    if (item !== undefined) return { kind: "ready", item };
    const next = page.cursor;
    if (next === undefined) return { kind: "not-found" };
    if (seen.has(next)) {
      return {
        kind: "incomplete",
        error: new Error("server repeated pagination cursor"),
      };
    }
    seen.add(next);
    cursor = next;
  }
}

/**
 * Owns one target-resolution read for a route.
 *
 * Captures the requested ID before the first await, invalidates outstanding
 * lookups when the requested target or authority generation changes, and
 * exposes a token so a late result can never be committed over a newer target.
 */
export class TargetResolver<T> {
  #scope: RequestScope;
  #resolution: TargetResolution<T> = { kind: "not-requested" };

  constructor(key = "target") {
    this.#scope = new RequestScope(key);
  }

  get resolution(): TargetResolution<T> {
    return this.#resolution;
  }

  get scope(): RequestScope {
    return this.#scope;
  }

  /** The resolved record when the exact target is usable. */
  get item(): T | null {
    return this.#resolution.kind === "ready" ? this.#resolution.item : null;
  }

  /** True while the target is still being looked up. */
  get loading(): boolean {
    return this.#resolution.kind === "loading";
  }

  /**
   * Resolves `requestedId`. A changed target or authority generation clears the
   * previous resolution before the new read begins, so a page can never render
   * the previous target's record for the new one.
   */
  async resolve(
    requestedId: string,
    args: Omit<ResolveTargetArgs<T>, "requestedId">,
    options: { authorityGeneration?: number } = {},
  ): Promise<TargetResolution<T>> {
    const key = `${requestedId}::${options.authorityGeneration ?? 0}`;
    const changed = this.#scope.setKey(key);
    const token: RequestToken = this.#scope.begin();
    if (changed) this.#resolution = { kind: "not-requested" };
    this.#resolution = requestedId === ""
      ? { kind: "not-requested" }
      : { kind: "loading" };
    const resolution = await resolveRequestedTarget<T>({
      requestedId,
      ...args,
    });
    if (!this.#scope.isCurrent(token)) return this.#resolution;
    this.#scope.settle(token);
    this.#resolution = resolution;
    return resolution;
  }

  /** Clears the resolution, for example when the route target changes. */
  clear(): void {
    this.#scope.invalidate();
    this.#resolution = { kind: "not-requested" };
  }

  /** Ends the resolver permanently. */
  dispose(): void {
    this.#scope.dispose();
  }
}
