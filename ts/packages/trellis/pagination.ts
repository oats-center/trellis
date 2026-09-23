import {
  type AsyncResult,
  BaseError,
  type BaseErrorSchema,
  err,
  ok,
  type Result,
} from "@oatscenter/result";
import type { RequestOpts } from "./session.ts";

type CursorPage<TItem> = Readonly<{
  items: readonly TItem[];
  page: Readonly<{ nextCursor?: string }>;
}>;

export type PaginationErrorData = BaseErrorSchema & {
  type: "PaginationError";
  cursor: string;
};

/** Failure caused by an invalid cursor sequence returned during pagination. */
export class PaginationError extends BaseError<PaginationErrorData> {
  override readonly name = "PaginationError" as const;

  constructor(readonly cursor: string) {
    super("The paginated RPC returned a cursor that was already visited.", {
      context: { cursor },
    });
  }

  override toSerializable(): PaginationErrorData {
    return { ...this.baseSerializable(), type: this.name, cursor: this.cursor };
  }
}

type CursorCall = (
  input: unknown,
  options?: RequestOpts,
) => AsyncResult<unknown, BaseError>;

function pageIterator<TPage extends CursorPage<unknown>>(
  call: CursorCall,
  input: unknown,
  options?: RequestOpts,
): AsyncIterator<Result<TPage, BaseError>> {
  let cursor: string | undefined;
  let stopped = false;
  let inFlight: AbortController | undefined;
  let serial = Promise.resolve();
  const initialCursor = typeof input === "object" && input !== null &&
      "page" in input && typeof input.page === "object" &&
      input.page !== null &&
      "cursor" in input.page && typeof input.page.cursor === "string" &&
      input.page.cursor.length > 0
    ? input.page.cursor
    : undefined;
  const seen = new Set(initialCursor === undefined ? [] : [initialCursor]);

  const next = () => {
    const result = serial.then(
      async (): Promise<IteratorResult<Result<TPage, BaseError>>> => {
        if (stopped) return { value: undefined, done: true };

        const controller = new AbortController();
        inFlight = controller;
        const signal = options?.signal
          ? AbortSignal.any([options.signal, controller.signal])
          : controller.signal;
        const requestInput = cursor === undefined ? input ?? {} : {
          ...(typeof input === "object" && input !== null ? input : {}),
          page: {
            ...(typeof input === "object" && input !== null &&
                "page" in input && typeof input.page === "object" &&
                input.page !== null
              ? input.page
              : {}),
            cursor,
          },
        };
        const result = await call(requestInput, { ...options, signal });
        if (inFlight === controller) inFlight = undefined;
        if (stopped) return { value: undefined, done: true };
        if (result.isErr()) {
          stopped = true;
          return { value: result, done: false };
        }

        const page = result.take() as TPage;
        const nextCursor = page.page.nextCursor;
        if (nextCursor === undefined) {
          stopped = true;
        } else if (seen.has(nextCursor)) {
          stopped = true;
          return { value: err(new PaginationError(nextCursor)), done: false };
        } else {
          seen.add(nextCursor);
          cursor = nextCursor;
        }
        return { value: ok<TPage, BaseError>(page), done: false };
      },
    );
    serial = result.then(() => undefined, () => undefined);
    return result;
  };

  return {
    next,
    async return() {
      stopped = true;
      inFlight?.abort();
      return { value: undefined, done: true };
    },
  };
}

/** Lazily traverses complete pages from one cursor-paginated RPC. */
export function cursorPages<TPage extends CursorPage<unknown>>(
  call: CursorCall,
  input?: unknown,
  options?: RequestOpts,
): AsyncIterable<Result<TPage, BaseError>> {
  return { [Symbol.asyncIterator]: () => pageIterator(call, input, options) };
}

/** Lazily traverses items from one cursor-paginated RPC. */
export function cursorItems<TItem, TPage extends CursorPage<TItem>>(
  call: CursorCall,
  input?: unknown,
  options?: RequestOpts,
): AsyncIterable<Result<TItem, BaseError>> {
  return {
    [Symbol.asyncIterator]() {
      const pages = pageIterator<TPage>(call, input, options);
      let items: Iterator<TItem> = [][Symbol.iterator]();
      return {
        async next(): Promise<IteratorResult<Result<TItem, BaseError>>> {
          while (true) {
            const item = items.next();
            if (!item.done) {
              return { value: ok<TItem, BaseError>(item.value), done: false };
            }
            const page = await pages.next();
            if (page.done) return { value: undefined, done: true };
            if (page.value.isErr()) return { value: page.value, done: false };
            items = page.value.orThrow().items[Symbol.iterator]();
          }
        },
        async return() {
          await pages.return?.();
          return { value: undefined, done: true };
        },
      };
    },
  };
}
