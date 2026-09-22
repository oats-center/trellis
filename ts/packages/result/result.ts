/**
 * A class-based Result<T, E> system inspired by Rust's Result type.
 *
 * This module provides Result and AsyncResult classes for elegant error handling
 * with method chaining and the `take()` pattern for early returns.
 *
 * @module @qlever-llc/result
 */

import type { BaseError } from "./error.ts";
import { UnexpectedError } from "./error.ts";

/**
 * Represents a successful result containing a value.
 */
export interface OkValue<T> {
  readonly success: true;
  readonly value: T;
}

/**
 * Represents a failed result containing an error.
 */
export interface ErrValue<E extends BaseError> {
  readonly success: false;
  readonly error: E;
}

/**
 * Internal type representing the raw result data.
 */
type ResultValue<T, E extends BaseError> = OkValue<T> | ErrValue<E>;

function isOkValue<T, E extends BaseError>(
  value: ResultValue<T, E>,
): value is OkValue<T> {
  return value.success;
}

function isErrValue<T, E extends BaseError>(
  value: ResultValue<T, E>,
): value is ErrValue<E> {
  return !value.success;
}

/**
 * Extract Result<never, E> types from a union while preserving specific error types.
 * This is a distributive conditional type that processes each member of the union separately.
 * For `string | Result<never, E1> | Result<never, E2>`, returns `Result<never, E1> | Result<never, E2>`.
 * The distribution over union members is key to preserving specific error types.
 */
type ExtractErrResult<U> = U extends Result<never, infer E> ? Result<never, E>
  : never;

/**
 * Extracts the Ok type T from a Result<T, E>.
 *
 * @example
 * ```typescript
 * type MyResult = Result<number, ValidationError>;
 * type Value = Infer<MyResult>; // number
 * ```
 */
export type Infer<R> = R extends Result<infer T, BaseError> ? T : never;

/**
 * Extracts the Err type E from a Result<T, E>.
 *
 * @example
 * ```typescript
 * type MyResult = Result<number, ValidationError>;
 * type Error = InferErr<MyResult>; // ValidationError
 * ```
 */
export type InferErr<R> = R extends Result<unknown, infer E> ? E : never;

/**
 * A type that accepts either a Result or AsyncResult with the same T and E types.
 *
 * This allows functions to return either synchronous or asynchronous results
 * interchangeably, making it easy to optimize or refactor without changing signatures.
 *
 * @example
 * ```typescript
 * function getUser(id: string): MaybeAsync<User, NotFoundError> {
 *   if (cache.has(id)) {
 *     return Result.ok(cache.get(id)); // Synchronous return
 *   }
 *   return AsyncResult.try(async () => {
 *     return await fetchUser(id); // Asynchronous return
 *   });
 * }
 * ```
 */
export type MaybeAsync<T, E extends BaseError> =
  | Result<T, E>
  | AsyncResult<T, E>
  | Promise<Result<T, E>>;

/**
 * A synchronous Result class that represents either success (Ok) or failure (Err).
 *
 * Provides method chaining for transformations and the `take()` pattern for
 * unwrapping values with early returns.
 *
 * @template T - The type of the success value
 * @template E - The type of the error (must extend BaseError)
 *
 * @example
 * ```typescript
 * function divide(a: number, b: number): Result<number, ValidationError> {
 *   if (b === 0) {
 *     return Result.err(new ValidationError("Division by zero"));
 *   }
 *   return Result.ok(a / b);
 * }
 *
 * const result = divide(10, 2)
 *   .map(x => x * 2)
 *   .map(x => x + 1);
 *
 * const value = result.take();
 * if (isErr(value)) return value;
 * console.log(value); // 11
 * ```
 */
export class Result<T, E extends BaseError> {
  private constructor(private readonly _value: ResultValue<T, E>) {}

  /**
   * Creates a successful Result containing a value.
   *
   * @template T - The type of the success value
   * @template E - The type of the error (defaults to never)
   * @param value - The success value to wrap
   * @returns A Result instance containing the value
   *
   * @example
   * ```typescript
   * const result = Result.ok(42);
   * const value = result.take();
   * if (!isErr(value)) {
   *   console.log(value); // 42
   * }
   * ```
   */
  static ok<T, E extends BaseError = never>(value: T): Result<T, E> {
    return new Result<T, E>({ success: true, value });
  }

  /**
   * Creates a failed Result containing an error.
   *
   * @template E - The type of the error (must extend BaseError)
   * @template T - The type of the success value (defaults to never)
   * @param error - The error to wrap
   * @returns A Result instance containing the error
   *
   * @example
   * ```typescript
   * const result = Result.err(new ValidationError("Invalid input"));
   * const value = result.take();
   * if (isErr(value)) {
   *   console.error(value.error.message); // "Invalid input"
   * }
   * ```
   */
  static err<E extends BaseError, T = never>(error: E): Result<T, E> {
    return new Result<T, E>({ success: false, error });
  }

  /**
   * Wraps a function that might throw into a Result.
   *
   * Catches any exceptions and wraps them in UnexpectedError.
   *
   * @template T - The type of the return value
   * @param fn - Function that might throw
   * @param context - Optional context to add to the error
   * @returns Ok with the return value, or Err with UnexpectedError
   *
   * @example
   * ```typescript
   * const obj = Result.try(() =>
   *   typeof data === "string" ? JSON.parse(data) : data
   * );
   *
   * const value = obj.take();
   * if (isErr(value)) {
   *   console.error("Parse failed:", value.error);
   *   return;
   * }
   * console.log(value); // parsed object
   * ```
   */
  static try<T>(
    fn: () => T,
    context?: Record<string, unknown>,
  ): Result<T, UnexpectedError> {
    try {
      return Result.ok(fn());
    } catch (cause) {
      return Result.err(new UnexpectedError({ cause }).withContext(context));
    }
  }

  /**
   * Type guard to check if a value is an Ok Result.
   *
   * @template T - The type of the success value
   * @template E - The type of the error
   * @param result - The Result to check
   * @returns True if the result is Ok, false otherwise
   *
   * @example
   * ```typescript
   * const result = Result.ok(42);
   * if (Result.isOk(result)) {
   *   // TypeScript knows result is Ok<number> here
   * }
   * ```
   */
  static isOk<T, E extends BaseError>(
    result: Result<T, E>,
  ): result is Result<T, never> {
    return result.isOk();
  }

  /**
   * Type guard to check if a value is an Err Result.
   *
   * This function has multiple overloads:
   * 1. Check if a Result is Err
   * 2. Check if any value (including from take()) is Err
   *
   * @template T - The type of the success value
   * @template E - The type of the error
   * @param value - The value to check (Result or unknown)
   * @returns True if the value is Err, false otherwise
   *
   * @example
   * ```typescript
   * const result = Result.err(new ValidationError("Failed"));
   * if (Result.isErr(result)) {
   *   console.log(result.error.message);
   * }
   *
   * // Works with take() output
   * const value = result.take();
   * if (Result.isErr(value)) {
   *   return value; // Early return with error Result
   * }
   * // TypeScript knows value is T here
   * ```
   */
  static isErr<T, E extends BaseError>(
    result: Result<T, E>,
  ): result is Result<never, E>;
  static isErr<T, E extends BaseError>(
    value: T | Result<never, E>,
  ): value is Result<never, E>;
  static isErr<T, E extends BaseError>(
    value: T | Result<T, E>,
  ): value is Result<never, E> {
    // Check if it's a Result instance
    if (value instanceof Result) {
      return value.isErr();
    }

    // For non-Result values, return false
    return false;
  }

  /**
   * Combines multiple Results into a single Result containing an array.
   *
   * If all Results are Ok, returns Ok with an array of all values.
   * If any Result is Err, returns the first Err encountered.
   *
   * @template T - The type of the Ok values
   * @template E - The type of the errors
   * @param results - Array of Results to combine
   * @returns Ok with array of values, or the first Err
   *
   * @example
   * ```typescript
   * const results = [Result.ok(1), Result.ok(2), Result.ok(3)];
   * const combined = Result.all(results);
   * // Ok([1, 2, 3])
   *
   * const withError = [Result.ok(1), Result.err(new ValidationError("Failed")), Result.ok(3)];
   * const combined2 = Result.all(withError);
   * // Err(ValidationError)
   * ```
   */
  static all<T, E extends BaseError>(
    results: readonly Result<T, E>[],
  ): Result<T[], E> {
    const values: T[] = [];
    for (const result of results) {
      if (result.isErr()) {
        return result as Result<T[], E>;
      }
      const resultValue = result._unsafeValue();
      if (resultValue.success) {
        values.push(resultValue.value);
      }
    }
    return Result.ok(values);
  }

  /**
   * Returns the first Ok result from an array of Results.
   *
   * If any Result is Ok, returns that Ok result.
   * If all Results are Err, returns the last Err.
   *
   * @template T - The type of the Ok values
   * @template E - The type of the errors
   * @param results - Array of Results to check
   * @returns The first Ok, or the last Err if all failed
   *
   * @example
   * ```typescript
   * const results = [Result.err(new Error("e1")), Result.ok(2), Result.ok(3)];
   * const first = Result.any(results);
   * // Ok(2)
   *
   * const allErrors = [
   *   Result.err(new Error("e1")),
   *   Result.err(new Error("e2")),
   *   Result.err(new Error("e3"))
   * ];
   * const first2 = Result.any(allErrors);
   * // Err(Error("e3")) - the last error
   * ```
   */
  static any<T, E extends BaseError>(
    results: readonly Result<T, E>[],
  ): Result<T, E> {
    for (const result of results) {
      if (result.isOk()) {
        return result;
      }
    }
    const last = results[results.length - 1];
    if (!last) {
      throw new Error("Result.any requires at least one result");
    }
    return last;
  }

  /**
   * Type guard to check if this Result is Ok.
   *
   * @returns True if this result is Ok, false otherwise
   *
   * @example
   * ```typescript
   * const result = Result.ok(42);
   * if (result.isOk()) {
   *   // TypeScript knows result is Ok here
   * }
   * ```
   */
  isOk(): this is Result<T, never> {
    return this._value.success === true;
  }

  /**
   * Type guard to check if this Result is Err.
   *
   * @returns True if this result is Err, false otherwise
   *
   * @example
   * ```typescript
   * const result = Result.err(new ValidationError("Failed"));
   * if (result.isErr()) {
   *   // TypeScript knows result is Err here
   * }
   * ```
   */
  isErr(): this is Result<never, E> {
    return this._value.success === false;
  }

  /**
   * Transforms the Ok value using a mapper function, leaving Err untouched.
   *
   * @template U - The type of the transformed value
   * @param fn - Function to transform the Ok value
   * @returns A new Result with the transformed value, or the original Err
   *
   * @example
   * ```typescript
   * const result = Result.ok(5)
   *   .map(x => x * 2)
   *   .map(x => x.toString());
   *
   * const value = result.take();
   * if (!isErr(value)) {
   *   console.log(value); // "10"
   * }
   * ```
   */
  map<U>(fn: (value: T) => U): Result<U, E> {
    const value = this._value;
    if (isOkValue(value)) {
      return Result.ok(fn(value.value));
    }
    return Result.err(value.error);
  }

  /**
   * Transforms the Err value using a mapper function, leaving Ok untouched.
   *
   * @template F - The type of the transformed error
   * @param fn - Function to transform the Err value
   * @returns A new Result with the transformed error, or the original Ok
   *
   * @example
   * ```typescript
   * const result = Result.err(new ValidationError("Failed"))
   *   .mapErr(e => new NetworkError({ cause: e }));
   * ```
   */
  mapErr<F extends BaseError>(fn: (error: E) => F): Result<T, F> {
    const value = this._value;
    if (isOkValue(value)) {
      return Result.ok(value.value);
    }
    return Result.err(fn(value.error));
  }

  /**
   * Chains operations that return Results (also known as flatMap).
   *
   * If this Result is Ok, calls the function with the Ok value and returns its Result.
   * If this Result is Err, returns the Err without calling the function.
   *
   * @template U - The type of the new Ok value
   * @template F - The type of the new error
   * @param fn - Function that takes the Ok value and returns a new Result
   * @returns The Result from calling fn, or the original Err
   *
   * @example
   * ```typescript
   * function parseNumber(s: string): Result<number, ValidationError> {
   *   const n = Number(s);
   *   if (isNaN(n)) {
   *     return Result.err(new ValidationError("Not a number"));
   *   }
   *   return Result.ok(n);
   * }
   *
   * const result = Result.ok("42")
   *   .andThen(parseNumber)
   *   .map(x => x * 2);
   * ```
   */
  andThen<U, F extends BaseError>(
    fn: (value: T) => Result<U, F>,
  ): Result<U, E | F> {
    const value = this._value;
    if (isOkValue(value)) {
      return fn(value.value) as Result<U, E | F>;
    }
    return Result.err(value.error) as Result<U, E | F>;
  }

  /**
   * Extracts the value from Ok or returns the Err for early returns.
   *
   * This is the equivalent of Rust's `?` operator. Use it with `isErr()` for
   * early returns in functions that return Results.
   *
   * Returns either:
   * - The unwrapped value T if this result is Ok
   * - An Err Result<never, E> if this result is Err (can be directly returned)
   *
   * @returns The unwrapped value or Err Result
   *
   * @example
   * ```typescript
   * function processData(input: string): Result<number, ValidationError> {
   *   const parsed = parseInput(input).take();
   *   if (isErr(parsed)) return parsed;
   *
   *   const validated = validate(parsed).take();
   *   if (isErr(validated)) return validated;
   *
   *   return Result.ok(validated * 2);
   * }
   * ```
   */
  take(): [T] extends [never] ? Result<never, E> : T | Result<never, E> {
    const value = this._value;
    if (isOkValue(value)) {
      return value.value as [T] extends [never] ? Result<never, E>
        : T | Result<never, E>;
    }
    return Result.err(value.error) as [T] extends [never] ? Result<never, E>
      : T | Result<never, E>;
  }

  /**
   * Returns the Ok value or throws the Err error.
   *
   * This is useful at process or demo boundaries where exception-style control
   * flow is acceptable and a caller wants to avoid repetitive `take()` /
   * `isErr(...)` branching.
   *
   * @returns The unwrapped Ok value
   * @throws The contained Err error
   */
  orThrow(): T {
    const value = this._value;
    if (isOkValue(value)) {
      return value.value;
    }
    throw value.error;
  }

  /**
   * Adds context to an Err result for early returns.
   * Chainable with take() for adding context when propagating errors.
   *
   * @param message - Context message describing the operation that failed
   * @param extra - Optional additional context data
   * @returns This Result with context added to the error
   *
   * @example
   * ```typescript
   * const user = await getUser(id).take();
   * if (isErr(user)) return user.context("failed to fetch user");
   *
   * // With extra data:
   * if (isErr(user)) return user.context("failed to fetch user", { userId: id });
   * ```
   */
  context(message: string, extra?: Record<string, unknown>): Result<T, E> {
    const value = this._value;
    if (isErrValue(value)) {
      const contextData = extra ? { message, ...extra } : { message };
      value.error.withContext(contextData);
    }
    return this;
  }

  /**
   * Pattern matching for Results - handle both Ok and Err cases.
   *
   * @template U - The type of the return value
   * @param pattern - Object with ok and err handler functions
   * @returns The result of calling either the ok or err handler
   *
   * @example
   * ```typescript
   * const message = result.match({
   *   ok: (value) => `Success: ${value}`,
   *   err: (error) => `Error: ${error.message}`
   * });
   * ```
   */
  match<U>(pattern: { ok: (value: T) => U; err: (error: E) => U }): U {
    const value = this._value;
    if (isOkValue(value)) {
      return pattern.ok(value.value);
    }
    return pattern.err(value.error);
  }

  /**
   * Returns the Ok value or a default value if Err.
   *
   * @template U - The type of the default value
   * @param defaultValue - The value to return if this result is Err
   * @returns The Ok value or the default value
   *
   * @example
   * ```typescript
   * const value = result.unwrapOr(0);
   * console.log(value); // 42 or 0
   * ```
   */
  unwrapOr<U>(defaultValue: U): T | U {
    const value = this._value;
    if (isOkValue(value)) {
      return value.value;
    }
    return defaultValue;
  }

  /**
   * Returns the Ok value or computes a default from the error.
   *
   * @template U - The type of the default value
   * @param fn - Function to compute the default value from the error
   * @returns The Ok value or the computed default value
   *
   * @example
   * ```typescript
   * const value = result.unwrapOrElse(error => {
   *   console.error(error);
   *   return 0;
   * });
   * ```
   */
  unwrapOrElse<U>(fn: (error: E) => U): T | U {
    const value = this._value;
    if (isOkValue(value)) {
      return value.value;
    }
    return fn(value.error);
  }

  /**
   * Returns this result if Ok, otherwise returns the fallback result.
   *
   * @template U - The type of the fallback Ok value
   * @param other - The fallback Result to use if this is Err
   * @returns This result if Ok, otherwise the fallback
   *
   * @example
   * ```typescript
   * const result = fetchFromCache()
   *   .or(fetchFromDatabase())
   *   .or(fetchFromAPI());
   * ```
   */
  or<U>(other: Result<U, E>): Result<T | U, E> {
    if (isOkValue(this._value)) {
      return this as Result<T | U, E>;
    }
    return other as Result<T | U, E>;
  }

  /**
   * Returns this result if Ok, otherwise computes a fallback from the error.
   *
   * @template R - The Result type returned by the fallback function
   * @param fn - Function to compute a fallback Result from the error
   * @returns This result if Ok, otherwise the computed fallback
   *
   * @example
   * ```typescript
   * const result = fetchData().orElse(error => {
   *   console.warn("Primary failed, trying backup");
   *   return fetchBackup();
   * });
   * ```
   */
  orElse<U, F extends BaseError>(
    fn: (error: E) => Result<U, F>,
  ): Result<T | U, F> {
    const value = this._value;
    if (isOkValue(value)) {
      return Result.ok(value.value) as Result<T | U, F>;
    }
    return fn(value.error) as Result<T | U, F>;
  }

  /**
   * Performs a side effect on the Ok value without changing the Result.
   *
   * @param fn - Function to call with the Ok value (if Ok)
   * @returns This Result, unchanged
   *
   * @example
   * ```typescript
   * const result = fetchUser("123")
   *   .inspect(user => console.log("Fetched:", user))
   *   .map(user => user.name);
   * ```
   */
  inspect(fn: (value: T) => void): Result<T, E> {
    const value = this._value;
    if (isOkValue(value)) {
      fn(value.value);
    }
    return this;
  }

  /**
   * Performs a side effect on the Err value without changing the Result.
   *
   * @param fn - Function to call with the error value (if Err)
   * @returns This Result, unchanged
   *
   * @example
   * ```typescript
   * const result = fetchUser("123")
   *   .inspectErr(error => console.error("Failed:", error))
   *   .map(user => user.name);
   * ```
   */
  inspectErr(fn: (error: E) => void): Result<T, E> {
    const value = this._value;
    if (isErrValue(value)) {
      fn(value.error);
    }
    return this;
  }

  /**
   * Gets the error from an Err Result.
   *
   * Only call this after checking `isErr()`. If called on Ok, throws an error.
   *
   * @returns The error value
   *
   * @example
   * ```typescript
   * const result = err(new ValidationError("Failed"));
   * if (result.isErr()) {
   *   console.log(result.error.message); // "Failed"
   * }
   * ```
   */
  get error(): E {
    const value = this._value;
    if (isErrValue(value)) {
      return value.error;
    }
    throw new Error("Called .error on an Ok Result");
  }

  /**
   * Internal method to get the raw value (for testing/debugging).
   * Not recommended for general use - prefer take() instead.
   */
  _unsafeValue(): ResultValue<T, E> {
    return this._value;
  }
}

/**
 * An asynchronous Result class that represents a Promise of Result<T, E>.
 *
 * Implements PromiseLike to be awaitable, and provides async versions of
 * all Result methods that return AsyncResult for seamless chaining.
 *
 * @template T - The type of the success value
 * @template E - The type of the error (must extend BaseError)
 *
 * @example
 * ```typescript
 * async function fetchUser(id: string): AsyncResult<User, NetworkError> {
 *   return AsyncResult.wrap(async () => {
 *     const response = await fetch(`/api/users/${id}`);
 *     return await response.json();
 *   });
 * }
 *
 * const result = fetchUser("123")
 *   .map(user => user.name)
 *   .map(name => name.toUpperCase());
 *
 * const value = await result.take();
 * if (isErr(value)) return value;
 * console.log(value); // "ALICE"
 * ```
 */
export class AsyncResult<T, E extends BaseError>
  implements PromiseLike<Result<T, E>> {
  constructor(private readonly promise: Promise<Result<T, E>>) {}

  /**
   * Creates an AsyncResult from a Promise of Result.
   *
   * @template T - The type of the success value
   * @template E - The type of the error
   * @param promise - The promise that resolves to a Result
   * @returns An AsyncResult wrapping the promise
   *
   * @example
   * ```typescript
   * const asyncResult = AsyncResult.from(fetchData());
   * ```
   */
  static from<T, E extends BaseError>(
    promise: Promise<Result<T, E>>,
  ): AsyncResult<T, E> {
    return new AsyncResult(promise);
  }

  /**
   * Creates a successful AsyncResult with the given value.
   *
   * @template T - The type of the Ok value
   * @template E - The type of the error (defaults to never)
   * @param value - The value to wrap in an Ok AsyncResult
   * @returns AsyncResult in the Ok state
   *
   * @example
   * ```typescript
   * const result = AsyncResult.ok(42);
   * const value = await result.take();
   * console.log(value); // 42
   * ```
   */
  static ok<T, E extends BaseError = never>(value: T): AsyncResult<T, E> {
    return new AsyncResult(Promise.resolve(Result.ok(value)));
  }

  /**
   * Creates a failed AsyncResult with the given error.
   *
   * @template E - The type of the error
   * @template T - The type of the Ok value (defaults to never)
   * @param error - The error to wrap in an Err AsyncResult
   * @returns AsyncResult in the Err state
   *
   * @example
   * ```typescript
   * const result = AsyncResult.err(new ValidationError("Invalid input"));
   * const value = await result.take();
   * if (Result.isErr(value)) {
   *   console.error(value.error.message); // "Invalid input"
   * }
   * ```
   */
  static err<E extends BaseError, T = never>(error: E): AsyncResult<T, E> {
    return new AsyncResult(Promise.resolve(Result.err(error)));
  }

  /**
   * Creates an AsyncResult from a Result, AsyncResult, or Promise<Result>.
   *
   * This is the key method for working with MaybeAsync types - it normalizes
   * both synchronous Results and asynchronous AsyncResults into AsyncResults.
   *
   * @template T - The type of the success value
   * @template E - The type of the error
   * @param value - A Result, AsyncResult, or Promise<Result>
   * @returns An AsyncResult
   *
   * @example
   * ```typescript
   * const asyncResult = AsyncResult.lift(Result.ok(42));
   * const asyncResult2 = AsyncResult.lift(Promise.resolve(Result.ok(42)));
   * const asyncResult3 = AsyncResult.lift(existingAsyncResult); // Pass-through
   * ```
   */
  static lift<T, E extends BaseError>(
    value: Result<T, E> | AsyncResult<T, E> | Promise<Result<T, E>>,
  ): AsyncResult<T, E> {
    if (value instanceof AsyncResult) {
      return value; // Already an AsyncResult, just return it
    }
    if (value instanceof Promise) {
      return new AsyncResult(value);
    }
    return new AsyncResult(Promise.resolve(value));
  }

  /**
   * Wraps an async function that might throw into an AsyncResult.
   *
   * Catches any exceptions and wraps them in UnexpectedError.
   *
   * @template T - The type of the return value
   * @param fn - Async function that might throw
   * @param context - Optional context to add to the error
   * @returns AsyncResult with the return value or UnexpectedError
   *
   * @example
   * ```typescript
   * const user = AsyncResult.try(async () => {
   *   const response = await fetch("/api/user");
   *   return await response.json();
   * });
   *
   * const value = await user.take();
   * if (isErr(value)) {
   *   console.error("Fetch failed:", value.error);
   *   return;
   * }
   * console.log(value); // user object
   * ```
   */
  static try<T>(
    fn: () => Promise<T>,
    context?: Record<string, unknown>,
  ): AsyncResult<T, UnexpectedError> {
    return new AsyncResult(
      (async () => {
        try {
          const value = await fn();
          return Result.ok(value);
        } catch (cause) {
          return Result.err(
            new UnexpectedError({ cause }).withContext(context),
          );
        }
      })(),
    );
  }

  /**
   * Combines multiple AsyncResults into a single AsyncResult containing an array.
   *
   * If all Results are Ok, returns Ok with an array of all values.
   * If any Result is Err, returns the first Err encountered.
   *
   * @template T - The type of the Ok values
   * @template E - The type of the errors
   * @param results - Array of AsyncResults or Promises to combine
   * @returns AsyncResult with array of values, or the first Err
   *
   * @example
   * ```typescript
   * const users = await AsyncResult.all([
   *   fetchUser("1"),
   *   fetchUser("2"),
   *   fetchUser("3")
   * ]).take();
   *
   * if (Result.isErr(users)) {
   *   console.error("Failed to fetch users");
   * } else {
   *   console.log(users); // [user1, user2, user3]
   * }
   * ```
   */
  static all<T, E extends BaseError>(
    results: readonly (AsyncResult<T, E> | Promise<Result<T, E>>)[],
  ): AsyncResult<T[], E> {
    return new AsyncResult(
      (async () => {
        const resolvedResults = await Promise.all(
          results.map(async (r) => {
            if (r instanceof AsyncResult) {
              const res = await r;
              return res;
            }
            return r;
          }),
        );

        const values: T[] = [];
        for (const result of resolvedResults) {
          if (result.isErr()) {
            return result as Result<T[], E>;
          }
          const resultValue = result._unsafeValue();
          if (resultValue.success) {
            values.push(resultValue.value);
          }
        }
        return Result.ok(values);
      })(),
    );
  }

  /**
   * Returns the first Ok result from an array of AsyncResults.
   *
   * If any Result is Ok, returns that Ok result.
   * If all Results are Err, returns the last Err.
   *
   * @template T - The type of the Ok values
   * @template E - The type of the errors
   * @param results - Array of AsyncResults or Promises to check
   * @returns AsyncResult with the first Ok, or the last Err
   *
   * @example
   * ```typescript
   * const data = await AsyncResult.any([
   *   fetchFromPrimary(),
   *   fetchFromSecondary(),
   *   fetchFromBackup()
   * ]).take();
   *
   * if (Result.isErr(data)) {
   *   console.error("All sources failed");
   * } else {
   *   console.log(data); // First successful result
   * }
   * ```
   */
  static any<T, E extends BaseError>(
    results: readonly (AsyncResult<T, E> | Promise<Result<T, E>>)[],
  ): AsyncResult<T, E> {
    return new AsyncResult(
      (async () => {
        const resolvedResults = await Promise.all(
          results.map(async (r) => {
            if (r instanceof AsyncResult) {
              const res = await r;
              return res;
            }
            return r;
          }),
        );

        for (const result of resolvedResults) {
          if (result.isOk()) {
            return result;
          }
        }
        const last = resolvedResults[resolvedResults.length - 1];
        if (!last) {
          throw new Error("AsyncResult.any requires at least one result");
        }
        return last;
      })(),
    );
  }

  /**
   * Implements PromiseLike to make AsyncResult awaitable.
   *
   * @template TResult1 - The type when fulfilled
   * @template TResult2 - The type when rejected
   * @param onfulfilled - Callback for when the promise is fulfilled
   * @param onrejected - Callback for when the promise is rejected
   * @returns A Promise of the result
   */
  then<TResult1 = Result<T, E>, TResult2 = never>(
    onfulfilled?:
      | ((value: Result<T, E>) => TResult1 | PromiseLike<TResult1>)
      | null,
    onrejected?: ((reason: unknown) => TResult2 | PromiseLike<TResult2>) | null,
  ): Promise<TResult1 | TResult2> {
    return this.promise.then(onfulfilled, onrejected);
  }

  /**
   * Transforms the Ok value using a mapper function, leaving Err untouched.
   *
   * @template U - The type of the transformed value
   * @param fn - Function to transform the Ok value
   * @returns A new AsyncResult with the transformed value
   *
   * @example
   * ```typescript
   * const result = fetchUser("123")
   *   .map(user => user.name)
   *   .map(name => name.toUpperCase());
   * ```
   */
  map<U>(fn: (value: T) => U): AsyncResult<U, E> {
    return new AsyncResult(this.promise.then((result) => result.map(fn)));
  }

  /**
   * Transforms the Err value using a mapper function, leaving Ok untouched.
   *
   * @template F - The type of the transformed error
   * @param fn - Function to transform the Err value
   * @returns A new AsyncResult with the transformed error
   *
   * @example
   * ```typescript
   * const result = fetchUser("123")
   *   .mapErr(e => new NetworkError({ cause: e }));
   * ```
   */
  mapErr<F extends BaseError>(fn: (error: E) => F): AsyncResult<T, F> {
    return new AsyncResult(this.promise.then((result) => result.mapErr(fn)));
  }

  /**
   * Chains operations that return Results.
   *
   * @template R - The Result type returned by the function
   * @param fn - Function that takes the Ok value and returns a Result, AsyncResult, or Promise
   * @returns A new AsyncResult from the chained operation
   *
   * @example
   * ```typescript
   * const result = fetchUser("123")
   *   .andThen(user => fetchPermissions(user.id));
   * ```
   */
  andThen<U, F extends BaseError>(
    fn: (value: T) => Result<U, F> | AsyncResult<U, F> | Promise<Result<U, F>>,
  ): AsyncResult<U, E | F> {
    return new AsyncResult(
      this.promise.then(async (result) => {
        const resultValue = result._unsafeValue();
        if (isErrValue(resultValue)) {
          return Result.err(resultValue.error);
        }
        const nextResult = fn(resultValue.value);
        if (nextResult instanceof AsyncResult) {
          return await nextResult;
        }
        return await nextResult;
      }),
    ) as AsyncResult<U, E | F>;
  }

  /**
   * Extracts the value from Ok or returns the Err for early returns.
   *
   * This is the async version of Result.take(). It returns a Promise that
   * resolves to either the unwrapped value T or an Err Result.
   *
   * @returns Promise of the unwrapped value or Err Result
   *
   * @example
   * ```typescript
   * async function processUser(id: string): Promise<Result<string, AppError>> {
   *   const user = await fetchUser(id).take();
   *   if (isErr(user)) return user;
   *
   *   const perms = await fetchPermissions(user.id).take();
   *   if (isErr(perms)) return perms;
   *
   *   return Result.ok(perms.join(", "));
   * }
   * ```
   */
  async take(): Promise<T | Result<never, E>> {
    const result = await this.promise;
    return result.take();
  }

  /**
   * Returns the Ok value or throws the Err error.
   *
   * Async version of `Result.orThrow()`.
   *
   * @returns Promise of the unwrapped Ok value
   * @throws The contained Err error
   */
  async orThrow(): Promise<T> {
    const result = await this.promise;
    return result.orThrow();
  }

  /**
   * Adds context to an Err result for early returns.
   * Async version - can be chained before take().
   *
   * @param message - Context message describing the operation that failed
   * @param extra - Optional additional context data
   * @returns AsyncResult with context added to any error
   *
   * @example
   * ```typescript
   * const user = await fetchUser(id).context("failed to fetch user").take();
   * if (isErr(user)) return user;
   * ```
   */
  context(message: string, extra?: Record<string, unknown>): AsyncResult<T, E> {
    return new AsyncResult(
      this.promise.then((result) => {
        result.context(message, extra);
        return result;
      }),
    );
  }

  /**
   * Pattern matching for async Results.
   *
   * @template U - The type of the return value
   * @param pattern - Object with ok and err handler functions
   * @returns Promise of the result from calling either handler
   *
   * @example
   * ```typescript
   * const message = await fetchUser("123").match({
   *   ok: (user) => `Welcome, ${user.name}`,
   *   err: (error) => `Error: ${error.message}`
   * });
   * ```
   */
  async match<U>(pattern: {
    ok: (value: T) => U;
    err: (error: E) => U;
  }): Promise<U> {
    const result = await this.promise;
    return result.match(pattern);
  }

  /**
   * Returns the Ok value or a default value if Err.
   *
   * @template U - The type of the default value
   * @param defaultValue - The value to return if the result is Err
   * @returns Promise of the Ok value or the default value
   */
  async unwrapOr<U>(defaultValue: U): Promise<T | U> {
    const result = await this.promise;
    return result.unwrapOr(defaultValue);
  }

  /**
   * Returns the Ok value or computes a default from the error.
   *
   * @template U - The type of the default value
   * @param fn - Function to compute the default value from the error
   * @returns Promise of the Ok value or the computed default value
   */
  async unwrapOrElse<U>(fn: (error: E) => U): Promise<T | U> {
    const result = await this.promise;
    return result.unwrapOrElse(fn);
  }

  /**
   * Returns this result if Ok, otherwise returns the fallback.
   *
   * @template U - The type of the fallback Ok value
   * @param other - The fallback Result or AsyncResult
   * @returns AsyncResult of this or the fallback
   */
  or<U>(
    other: Result<U, E> | AsyncResult<U, E> | Promise<Result<U, E>>,
  ): AsyncResult<T | U, E> {
    return new AsyncResult(
      this.promise.then(async (result) => {
        if (result.isOk()) {
          return result as Result<T | U, E>;
        }
        if (other instanceof AsyncResult) {
          return (await other) as Result<T | U, E>;
        }
        return (await other) as Result<T | U, E>;
      }),
    );
  }

  /**
   * Returns this result if Ok, otherwise computes a fallback from the error.
   *
   * @template R - The Result or AsyncResult type returned by the fallback function
   * @param fn - Function to compute a fallback Result from the error
   * @returns AsyncResult of this or the computed fallback
   */
  orElse<U, F extends BaseError>(
    fn: (error: E) => Result<U, F> | AsyncResult<U, F> | Promise<Result<U, F>>,
  ): AsyncResult<T | U, F> {
    return new AsyncResult(
      this.promise.then(async (result) => {
        const resultValue = result._unsafeValue();
        if (isOkValue(resultValue)) {
          return Result.ok(resultValue.value);
        }
        const nextResult = fn(resultValue.error);
        if (nextResult instanceof AsyncResult) {
          return await nextResult;
        }
        return await nextResult;
      }),
    ) as AsyncResult<T | U, F>;
  }

  /**
   * Performs a side effect on the Ok value without changing the result.
   *
   * @param fn - Function to call with the Ok value (if Ok)
   * @returns This AsyncResult, unchanged
   */
  inspect(fn: (value: T) => void | Promise<void>): AsyncResult<T, E> {
    return new AsyncResult(
      this.promise.then(async (result) => {
        const resultValue = result._unsafeValue();
        if (isOkValue(resultValue)) {
          await fn(resultValue.value);
        }
        return result;
      }),
    );
  }

  /**
   * Performs a side effect on the Err value without changing the result.
   *
   * @param fn - Function to call with the error value (if Err)
   * @returns This AsyncResult, unchanged
   */
  inspectErr(fn: (error: E) => void | Promise<void>): AsyncResult<T, E> {
    return new AsyncResult(
      this.promise.then(async (result) => {
        const resultValue = result._unsafeValue();
        if (isErrValue(resultValue)) {
          await fn(resultValue.error);
        }
        return result;
      }),
    );
  }
}
