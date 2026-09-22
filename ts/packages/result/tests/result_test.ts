import { assertEquals, assertInstanceOf } from "@std/assert";
import { BaseError, UnexpectedError } from "@qlever-llc/result";
import {
  AsyncResult,
  type Infer,
  type InferErr,
  type MaybeAsync,
  Result,
} from "../result.ts";

/**
 * Simple test error class for testing purposes.
 */
class TestError extends BaseError {
  override readonly name = "TestError" as const;

  constructor(message: string) {
    super(message);
  }

  override toSerializable() {
    return {
      id: this.id,
      type: this.name,
      message: this.message,
      context: this.getContext(),
    };
  }
}

class ValidationError extends BaseError {
  override readonly name = "ValidationError" as const;

  constructor(message: string) {
    super(message);
  }

  override toSerializable() {
    return {
      id: this.id,
      type: this.name,
      message: this.message,
      context: this.getContext(),
    };
  }
}

Deno.test("Result class", async (t) => {
  await t.step("basic construction with static methods", () => {
    const success = Result.ok(42);
    const failure = Result.err(new TestError("oops"));

    assertEquals(success.isOk(), true);
    assertEquals(failure.isErr(), true);

    const value = success.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 42);
    }

    const error = failure.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "oops");
    }
  });

  await t.step("BaseError subclasses preserve prototype identity", () => {
    const error = new TestError("oops");
    assertInstanceOf(error, TestError);
    assertInstanceOf(error, BaseError);
    assertInstanceOf(error, Error);

    const unexpected = new UnexpectedError({ cause: new Error("boom") });
    assertInstanceOf(unexpected, UnexpectedError);
    assertInstanceOf(unexpected, BaseError);
    assertInstanceOf(unexpected, Error);
  });

  await t.step("construction with helper functions", () => {
    const success = Result.ok(42);
    const failure = Result.err(new TestError("oops"));

    assertEquals(Result.isOk(success), true);
    assertEquals(Result.isErr(failure), true);
  });

  await t.step("map transforms values", () => {
    const result = Result.ok(5).map((x) => x * 2).map((x) => x + 1);

    const value = result.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 11);
    }

    const error = Result.err<TestError, number>(new TestError("failed"));
    const mapped = error.map((x) => x * 2);
    assertEquals(mapped.isErr(), true);
  });

  await t.step("mapErr transforms errors", () => {
    const result = Result.err<TestError, number>(new TestError("failed"))
      .mapErr(
        (e) => new ValidationError(e.message),
      );

    const error = result.take();
    if (Result.isErr(error)) {
      assertInstanceOf(error.error, ValidationError);
      assertEquals(error.error.message, "failed");
    }

    const success = Result.ok<number, TestError>(42).mapErr(
      (e) => new ValidationError(e.message),
    );
    assertEquals(success.isOk(), true);
  });

  await t.step("andThen chains operations", () => {
    function divide(a: number, b: number): Result<number, TestError> {
      if (b === 0) {
        return Result.err(new TestError("Division by zero"));
      }
      return Result.ok(a / b);
    }

    const result = Result.ok(10).andThen((x) => divide(x, 2));
    const value = result.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 5);
    }

    const divByZero = Result.ok(10).andThen((x) => divide(x, 0));
    assertEquals(divByZero.isErr(), true);
    const error = divByZero.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "Division by zero");
    }
  });

  await t.step("take extracts value or error", () => {
    const success = Result.ok(5);
    const value = success.take();
    assertEquals(value, 5);

    const failure = Result.err(new TestError("failed"));
    const error = failure.take();
    assertEquals(Result.isErr(error), true);
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "failed");
    }
  });

  await t.step("orThrow returns Ok values and throws Err errors", () => {
    assertEquals(Result.ok(5).orThrow(), 5);

    let thrown: unknown;
    try {
      Result.err(new TestError("failed")).orThrow();
    } catch (error) {
      thrown = error;
    }

    assertInstanceOf(thrown, TestError);
    assertEquals((thrown as TestError).message, "failed");
  });

  await t.step("take with early return pattern", () => {
    function processValue(input: number): Result<string, TestError> {
      const doubled = Result.ok(input).map((x) => x * 2).take();
      if (Result.isErr(doubled)) return doubled;

      if (doubled > 100) {
        return Result.err(new TestError("Value too large"));
      }

      return Result.ok(doubled.toString());
    }

    const success = processValue(10);
    const value = success.take();
    if (!Result.isErr(value)) {
      assertEquals(value, "20");
    }

    const tooBig = processValue(60);
    const error = tooBig.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "Value too large");
    }
  });

  await t.step("match handles both cases", () => {
    const successMsg = Result.ok(42).match({
      ok: (v) => `Success: ${v}`,
      err: (e: TestError) => `Error: ${e.message}`,
    });
    assertEquals(successMsg, "Success: 42");

    const errorMsg = Result.err(new TestError("failed")).match({
      ok: (v) => `Success: ${v}`,
      err: (e) => `Error: ${e.message}`,
    });
    assertEquals(errorMsg, "Error: failed");
  });

  await t.step("unwrapOr provides default value", () => {
    const value = Result.ok(42).unwrapOr(0);
    assertEquals(value, 42);

    const defaultValue = Result.err<TestError, number>(new TestError("failed"))
      .unwrapOr(0);
    assertEquals(defaultValue, 0);
  });

  await t.step("unwrapOrElse computes default", () => {
    const value = Result.ok(42).unwrapOrElse(() => 0);
    assertEquals(value, 42);

    let errorSeen = false;
    const defaultValue = Result.err<TestError, number>(new TestError("failed"))
      .unwrapOrElse((e) => {
        errorSeen = true;
        assertEquals(e.message, "failed");
        return 0;
      });
    assertEquals(defaultValue, 0);
    assertEquals(errorSeen, true);
  });

  await t.step("or returns first Ok", () => {
    const result1 = Result.ok(1).or(Result.ok(2));
    const value1 = result1.take();
    if (!Result.isErr(value1)) {
      assertEquals(value1, 1);
    }

    const result2 = Result.err<TestError, number>(new TestError("e1")).or(
      Result.ok(2),
    );
    const value2 = result2.take();
    if (!Result.isErr(value2)) {
      assertEquals(value2, 2);
    }
  });

  await t.step("orElse computes fallback", () => {
    const result1 = Result.ok(1).orElse(() => Result.ok(2));
    const value1 = result1.take();
    if (!Result.isErr(value1)) {
      assertEquals(value1, 1);
    }

    const result2 = Result.err<TestError, number>(new TestError("e1")).orElse(
      (error) => {
        assertEquals(error.message, "e1");
        return Result.ok(2);
      },
    );
    const value2 = result2.take();
    if (!Result.isErr(value2)) {
      assertEquals(value2, 2);
    }
  });

  await t.step("inspect performs side effect on Ok", () => {
    let inspected = false;
    const result = Result.ok(42).inspect((v) => {
      inspected = true;
      assertEquals(v, 42);
    });
    assertEquals(inspected, true);
    assertEquals(result.isOk(), true);

    let errInspected = false;
    Result.err(new TestError("failed")).inspect(() => {
      errInspected = true;
    });
    assertEquals(errInspected, false);
  });

  await t.step("inspectErr performs side effect on Err", () => {
    let inspected = false;
    const result = Result.err(new TestError("failed")).inspectErr((e) => {
      inspected = true;
      assertEquals(e.message, "failed");
    });
    assertEquals(inspected, true);
    assertEquals(result.isErr(), true);

    let okInspected = false;
    Result.ok(42).inspectErr(() => {
      okInspected = true;
    });
    assertEquals(okInspected, false);
  });

  await t.step("all combines sync results", () => {
    const allOk = Result.all([Result.ok(1), Result.ok(2), Result.ok(3)]);
    const values = allOk.take();
    if (!Result.isErr(values)) {
      assertEquals(values, [1, 2, 3]);
    }

    const hasErr = Result.all([
      Result.ok(1),
      Result.err(new TestError("failed")),
      Result.ok(3),
    ]);
    assertEquals(hasErr.isErr(), true);
    const error = hasErr.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "failed");
    }
  });

  await t.step("any finds first Ok", () => {
    const first = Result.any([
      Result.err<TestError, number>(new TestError("e1")),
      Result.ok(2),
      Result.ok(3),
    ]);
    const value = first.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 2);
    }

    const allErrors = Result.any([
      Result.err<TestError, number>(new TestError("e1")),
      Result.err<TestError, number>(new TestError("e2")),
      Result.err<TestError, number>(new TestError("e3")),
    ]);
    assertEquals(allErrors.isErr(), true);
    const error = allErrors.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "e3");
    }
  });

  await t.step("Result.try catches exceptions", () => {
    const success = Result.try(() => JSON.parse('{"a":1}'));
    assertEquals(success.isOk(), true);
    const value = success.take();
    if (!Result.isErr(value)) {
      assertEquals(value.a, 1);
    }

    const failure = Result.try(() => JSON.parse("invalid"));
    assertEquals(failure.isErr(), true);
    const error = failure.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.name, "UnexpectedError");
    }
  });

  await t.step("Result.try with context", () => {
    const data = "invalid json";
    const result = Result.try(
      () => JSON.parse(data),
      { input: data },
    );

    assertEquals(result.isErr(), true);
    const error = result.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.name, "UnexpectedError");
    }
  });

  await t.step("Result.try practical example", () => {
    const data = '{"name":"Alice","age":30}';
    const obj = Result.try(() =>
      typeof data === "string" ? JSON.parse(data) : data
    );

    const value = obj.take();
    if (!Result.isErr(value)) {
      assertEquals(value.name, "Alice");
      assertEquals(value.age, 30);
    }
  });
});

Deno.test("AsyncResult class", async (t) => {
  await t.step("basic async construction", async () => {
    async function fetchUser(
      id: string,
    ): Promise<Result<string, TestError>> {
      await new Promise((resolve) => setTimeout(resolve, 10));

      if (id === "1") {
        return Result.ok("Alice");
      }
      return Result.err(new TestError("User not found"));
    }

    const asyncResult = AsyncResult.from(fetchUser("1"));
    const value = await asyncResult.take();
    assertEquals(value, "Alice");

    const asyncError = AsyncResult.from(fetchUser("999"));
    const error = await asyncError.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "User not found");
    }
  });

  await t.step("lift creates AsyncResult from Result or Promise", async () => {
    const syncResult = AsyncResult.lift(Result.ok(42));
    const value1 = await syncResult.take();
    assertEquals(value1, 42);

    const promiseResult = AsyncResult.lift(Promise.resolve(Result.ok(42)));
    const value2 = await promiseResult.take();
    assertEquals(value2, 42);
  });

  await t.step("map transforms async values", async () => {
    const result = AsyncResult.lift(Result.ok(5))
      .map((x) => x * 2)
      .map((x) => x + 1);

    const value = await result.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 11);
    }

    const error = AsyncResult.lift(
      Result.err<TestError, number>(new TestError("failed")),
    );
    const mapped = error.map((x) => x * 2);
    const errorValue = await mapped.take();
    if (Result.isErr(errorValue)) {
      assertEquals(errorValue.error.message, "failed");
    }
  });

  await t.step("mapErr transforms async errors", async () => {
    const result = AsyncResult.lift(
      Result.err<TestError, number>(new TestError("failed")),
    ).mapErr((e) => new ValidationError(e.message));

    const error = await result.take();
    if (Result.isErr(error)) {
      assertInstanceOf(error.error, ValidationError);
      assertEquals(error.error.message, "failed");
    }
  });

  await t.step("andThen chains async operations", async () => {
    async function getUser(
      id: string,
    ): Promise<Result<{ id: string; name: string }, TestError>> {
      await new Promise((resolve) => setTimeout(resolve, 10));
      if (id === "1") return Result.ok({ id, name: "Alice" });
      return Result.err(new TestError("Not found"));
    }

    async function getPermissions(user: {
      name: string;
    }): Promise<Result<string[], TestError>> {
      await new Promise((resolve) => setTimeout(resolve, 10));
      if (user.name === "Alice") return Result.ok(["read", "write"]);
      return Result.err(new TestError("No permissions"));
    }

    const permissions = AsyncResult.from(getUser("1")).andThen((user) =>
      AsyncResult.from(getPermissions(user))
    );

    const value = await permissions.take();
    if (!Result.isErr(value)) {
      assertEquals(value, ["read", "write"]);
    }
  });

  await t.step("andThen works with sync Results", async () => {
    function validateNumber(n: number): Result<number, TestError> {
      if (n < 0) return Result.err(new TestError("Negative number"));
      return Result.ok(n);
    }

    const result = AsyncResult.lift(Result.ok(5)).andThen(validateNumber);

    const value = await result.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 5);
    }

    const negative = AsyncResult.lift(Result.ok(-5)).andThen(validateNumber);
    const error = await negative.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "Negative number");
    }
  });

  await t.step("take with early return pattern", async () => {
    async function processUser(
      id: string,
    ): Promise<Result<string, TestError>> {
      async function fetchUser(
        id: string,
      ): Promise<Result<string, TestError>> {
        await new Promise((resolve) => setTimeout(resolve, 10));
        if (id === "1") return Result.ok("Alice");
        return Result.err(new TestError("User not found"));
      }

      const user = await AsyncResult.from(fetchUser(id)).take();
      if (Result.isErr(user)) return user;

      if (user.length < 3) {
        return Result.err(new TestError("Name too short"));
      }

      return Result.ok(user.toUpperCase());
    }

    const success = await processUser("1");
    const value = success.take();
    if (!Result.isErr(value)) {
      assertEquals(value, "ALICE");
    }

    const notFound = await processUser("999");
    const error = notFound.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "User not found");
    }
  });

  await t.step(
    "orThrow returns async Ok values and throws Err errors",
    async () => {
      assertEquals(await AsyncResult.ok(42).orThrow(), 42);

      let thrown: unknown;
      try {
        await AsyncResult.err(new TestError("failed")).orThrow();
      } catch (error) {
        thrown = error;
      }

      assertInstanceOf(thrown, TestError);
      assertEquals((thrown as TestError).message, "failed");
    },
  );

  await t.step("await AsyncResult returns Result", async () => {
    const asyncResult = AsyncResult.lift(Result.ok(42));
    const result = await asyncResult;

    assertEquals(result.isOk(), true);
    const value = result.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 42);
    }
  });

  await t.step("match handles both async cases", async () => {
    const successMsg = await AsyncResult.lift(Result.ok(42)).match({
      ok: (v) => `Success: ${v}`,
      err: (e: TestError) => `Error: ${e.message}`,
    });
    assertEquals(successMsg, "Success: 42");

    const errorMsg = await AsyncResult.lift(
      Result.err(new TestError("failed")),
    ).match({
      ok: (v) => `Success: ${v}`,
      err: (e) => `Error: ${e.message}`,
    });
    assertEquals(errorMsg, "Error: failed");
  });

  await t.step("unwrapOr provides default for async", async () => {
    const value = await AsyncResult.lift(Result.ok(42)).unwrapOr(0);
    assertEquals(value, 42);

    const defaultValue = await AsyncResult.lift(
      Result.err<TestError, number>(new TestError("failed")),
    ).unwrapOr(0);
    assertEquals(defaultValue, 0);
  });

  await t.step("unwrapOrElse computes default for async", async () => {
    const value = await AsyncResult.lift(Result.ok(42)).unwrapOrElse(() => 0);
    assertEquals(value, 42);

    const defaultValue = await AsyncResult.lift(
      Result.err<TestError, number>(new TestError("failed")),
    ).unwrapOrElse((e) => {
      assertEquals(e.message, "failed");
      return 0;
    });
    assertEquals(defaultValue, 0);
  });

  await t.step("or returns first Ok for async", async () => {
    const result1 = AsyncResult.lift(Result.ok(1)).or(
      AsyncResult.lift(Result.ok(2)),
    );
    const value1 = await result1.take();
    if (!Result.isErr(value1)) {
      assertEquals(value1, 1);
    }

    const result2 = AsyncResult.lift(
      Result.err<TestError, number>(new TestError("e1")),
    ).or(AsyncResult.lift(Result.ok(2)));
    const value2 = await result2.take();
    if (!Result.isErr(value2)) {
      assertEquals(value2, 2);
    }
  });

  await t.step("orElse computes fallback for async", async () => {
    const result1 = AsyncResult.lift(Result.ok(1)).orElse(() =>
      AsyncResult.lift(Result.ok(2))
    );
    const value1 = await result1.take();
    if (!Result.isErr(value1)) {
      assertEquals(value1, 1);
    }

    const result2 = AsyncResult.lift(
      Result.err<TestError, number>(new TestError("e1")),
    ).orElse((error) => {
      assertEquals(error.message, "e1");
      return AsyncResult.lift(Result.ok(2));
    });
    const value2 = await result2.take();
    if (!Result.isErr(value2)) {
      assertEquals(value2, 2);
    }
  });

  await t.step("inspect performs side effect on async Ok", async () => {
    let inspected = false;
    await AsyncResult.lift(Result.ok(42)).inspect((v) => {
      inspected = true;
      assertEquals(v, 42);
    });
    assertEquals(inspected, true);
  });

  await t.step("inspectErr performs side effect on async Err", async () => {
    let inspected = false;
    await AsyncResult.lift(Result.err(new TestError("failed"))).inspectErr(
      (e) => {
        inspected = true;
        assertEquals(e.message, "failed");
      },
    );
    assertEquals(inspected, true);
  });

  await t.step("all combines async results", async () => {
    const allOk = AsyncResult.all([
      AsyncResult.lift(Result.ok(1)),
      AsyncResult.lift(Result.ok(2)),
      AsyncResult.lift(Result.ok(3)),
    ]);
    const values = await allOk.take();
    if (!Result.isErr(values)) {
      assertEquals(values, [1, 2, 3]);
    }

    const hasErr = AsyncResult.all([
      AsyncResult.lift(Result.ok(1)),
      AsyncResult.lift(Result.err(new TestError("failed"))),
      AsyncResult.lift(Result.ok(3)),
    ]);
    const error = await hasErr.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "failed");
    }
  });

  await t.step("any finds first async Ok", async () => {
    const first = AsyncResult.any([
      AsyncResult.lift(Result.err<TestError, number>(new TestError("e1"))),
      AsyncResult.lift(Result.ok(2)),
      AsyncResult.lift(Result.ok(3)),
    ]);
    const value = await first.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 2);
    }

    const allErrors = AsyncResult.any([
      AsyncResult.lift(Result.err<TestError, number>(new TestError("e1"))),
      AsyncResult.lift(Result.err<TestError, number>(new TestError("e2"))),
      AsyncResult.lift(Result.err<TestError, number>(new TestError("e3"))),
    ]);
    const error = await allErrors.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "e3");
    }
  });

  await t.step("complex chaining example", async () => {
    interface User {
      id: string;
      name: string;
      age: number;
    }

    async function fetchUser(id: string): Promise<Result<User, TestError>> {
      await new Promise((resolve) => setTimeout(resolve, 10));
      if (id === "1") {
        return Result.ok({ id, name: "Alice", age: 30 });
      }
      return Result.err(new TestError("User not found"));
    }

    async function fetchPermissions(
      user: User,
    ): Promise<Result<string[], TestError>> {
      await new Promise((resolve) => setTimeout(resolve, 10));
      if (user.age >= 18) {
        return Result.ok(["read", "write"]);
      }
      return Result.err(new TestError("User too young"));
    }

    const result = AsyncResult.from(fetchUser("1"))
      .andThen((user) => AsyncResult.from(fetchPermissions(user)))
      .map((perms) => perms.join(", "));

    const value = await result.take();
    if (!Result.isErr(value)) {
      assertEquals(value, "read, write");
    }

    const notFound = AsyncResult.from(fetchUser("999")).andThen((user) =>
      AsyncResult.from(fetchPermissions(user))
    );
    const error = await notFound.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.message, "User not found");
    }
  });

  await t.step("multiple validations with early returns", async () => {
    function validateEmail(email: string): Result<string, TestError> {
      if (!email.includes("@")) {
        return Result.err(new TestError("Invalid email"));
      }
      return Result.ok(email);
    }

    function validatePassword(password: string): Result<string, TestError> {
      if (password.length < 8) {
        return Result.err(new TestError("Password too short"));
      }
      return Result.ok(password);
    }

    async function registerUser(
      email: string,
      password: string,
    ): Promise<Result<{ email: string }, TestError>> {
      const emailValue = validateEmail(email).take();
      if (Result.isErr(emailValue)) return emailValue;

      const passwordValue = validatePassword(password).take();
      if (Result.isErr(passwordValue)) return passwordValue;

      await new Promise((resolve) => setTimeout(resolve, 10));

      return Result.ok({ email: emailValue });
    }

    const user = await registerUser("test@example.com", "password123");
    const value = user.take();
    if (!Result.isErr(value)) {
      assertEquals(value.email, "test@example.com");
    }

    const badEmail = await registerUser("invalid", "password123");
    const error1 = badEmail.take();
    if (Result.isErr(error1)) {
      assertEquals(error1.error.message, "Invalid email");
    }

    const badPassword = await registerUser("test@example.com", "short");
    const error2 = badPassword.take();
    if (Result.isErr(error2)) {
      assertEquals(error2.error.message, "Password too short");
    }
  });

  await t.step("AsyncResult.try catches async exceptions", async () => {
    const success = AsyncResult.try(async () => {
      return Promise.resolve(42);
    });
    const value = await success.take();
    if (!Result.isErr(value)) {
      assertEquals(value, 42);
    }

    const failure = AsyncResult.try(async () => {
      throw new Error("async error");
    });
    const error = await failure.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.name, "UnexpectedError");
    }
  });

  await t.step("AsyncResult.try with fetch example", async () => {
    const mockFetch = async (url: string) => {
      if (url.includes("error")) {
        throw new Error("Network error");
      }
      return { json: async () => ({ name: "Alice", id: "123" }) };
    };

    const user = AsyncResult.try(async () => {
      const response = await mockFetch("/api/user");
      return await response.json();
    });

    const value = await user.take();
    if (!Result.isErr(value)) {
      assertEquals(value.name, "Alice");
      assertEquals(value.id, "123");
    }

    const error = AsyncResult.try(async () => {
      const response = await mockFetch("/api/error");
      return await response.json();
    });

    const errorValue = await error.take();
    if (Result.isErr(errorValue)) {
      assertEquals(errorValue.error.name, "UnexpectedError");
    }
  });

  await t.step("AsyncResult.try with context", async () => {
    const url = "/api/data";
    const result = AsyncResult.try(
      async () => {
        throw new Error("Failed to fetch");
      },
      { url },
    );

    const error = await result.take();
    if (Result.isErr(error)) {
      assertEquals(error.error.name, "UnexpectedError");
    }
  });
});

Deno.test("Type inference utilities", async (t) => {
  await t.step("Infer extracts Ok type", () => {
    type MyResult = Result<number, TestError>;
    type Value = Infer<MyResult>;

    // Compile-time type check
    const _typeCheck: Value = 42;
    assertEquals(_typeCheck, 42);
  });

  await t.step("InferErr extracts Err type", () => {
    type MyResult = Result<number, TestError>;
    type Error = InferErr<MyResult>;

    // Compile-time type check
    const _typeCheck: Error = new TestError("test");
    assertEquals(_typeCheck.name, "TestError");
  });

  await t.step("works with complex Result types", () => {
    type UserResult = Result<{ id: string; name: string }, ValidationError>;
    type User = Infer<UserResult>;
    type UserError = InferErr<UserResult>;

    const user: User = { id: "123", name: "Alice" };
    const error: UserError = new ValidationError("Failed");

    assertEquals(user.id, "123");
    assertEquals(error.name, "ValidationError");
  });

  await t.step("MaybeAsync allows Result or AsyncResult", async () => {
    function flexibleFetch(useCache: boolean): MaybeAsync<string, TestError> {
      if (useCache) {
        return Result.ok("cached");
      }
      return AsyncResult.lift(Result.ok("fetched"));
    }

    const cached = flexibleFetch(true);
    const fetched = flexibleFetch(false);

    // Verify types at runtime
    assertEquals(cached instanceof AsyncResult, false);
    assertInstanceOf(fetched, AsyncResult);

    // Both can be handled uniformly with AsyncResult.lift
    const val1 = await AsyncResult.lift(cached).take();
    const val2 = await AsyncResult.lift(fetched).take();

    assertEquals(val1, "cached");
    assertEquals(val2, "fetched");
  });
});
