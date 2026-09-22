/**
 * @qlever-llc/result - Class-based Result type for TypeScript/Deno
 *
 * A class-based Result<T, E> system inspired by Rust's Result type,
 * providing elegant error handling with method chaining and the `take()`
 * pattern for early returns.
 *
 * This library provides two main classes:
 * - `Result<T, E>`: Synchronous result type with method chaining
 * - `AsyncResult<T, E>`: Asynchronous result type that implements PromiseLike
 *
 * @example Basic usage with static methods
 * ```typescript
 * import { Result } from "@qlever-llc/result";
 *
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
 * if (Result.isErr(value)) return value;
 * console.log(value); // 11
 * ```
 *
 * @example Async operations
 * ```typescript
 * import { AsyncResult, Result } from "@qlever-llc/result";
 *
 * const user = AsyncResult.try(async () => {
 *   const response = await fetch(`/api/users/123`);
 *   return await response.json();
 * });
 *
 * const result = user
 *   .map(user => user.name)
 *   .map(name => name.toUpperCase());
 *
 * const value = await result.take();
 * if (Result.isErr(value)) return value;
 * console.log(value); // "ALICE"
 * ```
 *
 * @module
 */

import { Result as ResultClass } from "./result.ts";

export { AsyncResult, Result } from "./result.ts";
export type {
  ErrValue,
  Infer,
  InferErr,
  MaybeAsync,
  OkValue,
} from "./result.ts";

export const ok = ResultClass.ok;
export const err = ResultClass.err;
export const isOk = ResultClass.isOk;
export const isErr = ResultClass.isErr;

export {
  BaseError,
  UnexpectedError,
  UnexpectedErrorDataSchema,
} from "./error.ts";
export type {
  BaseErrorOptions,
  BaseErrorSchema,
  UnexpectedErrorData,
} from "./error.ts";
