# @qlever-llc/result

Class-based `Result<T, E>` and `AsyncResult<T, E>` types for TypeScript,
inspired by Rust.

The npm package contains ESM JavaScript and TypeScript declarations. Use
`import`; CommonJS output and `require()` support have intentionally been
removed. JSR publication remains supported.

Provides explicit error handling with method chaining and the `take()` pattern
for early returns, without relying on exceptions.

```typescript
import { AsyncResult, Result } from "@qlever-llc/result";

const user = await fetchUser(id).take();
if (Result.isErr(user)) return user;
```

Key surface area:

- `Result.ok(value)` / `Result.err(error)` — construction
- `Result.try(() => ...)` / `AsyncResult.try(async () => ...)` — wrap throwing
  code
- `.map()`, `.mapErr()`, `.andThen()` — transforms
- `.take()` — early return pattern (Rust's `?` operator equivalent)
- `.match({ ok, err })` — pattern matching
- `Result.all()` / `Result.any()` — combinators
- `MaybeAsync<T, E>` — flexible sync/async return type
- `BaseError` — base class for all Trellis error types

See the source and inline JSDoc for full API details.
