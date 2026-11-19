---
title: "Handle graceful shutdown"
description: "SIGTERM and Ctrl-C stop the service gracefully: it takes no new work, finishes what it holds, and hands the rest back."
---

# Stop the service

The walkthrough's `main.ts` needs no signal handling of its own. While
`service.wait()` runs, SIGTERM and Ctrl-C (SIGINT) stop the service gracefully,
and `wait()` returns:

```ts
await service.handleCreate(createOrder);
await service.wait();
```

A graceful stop takes no new requests, jobs or event deliveries, so other
instances get them, and ends the service's Live streams so their callers can
open them again elsewhere. It then waits up to `runtime.stopTimeoutMs` (25 s by
default) for the requests, deliveries and jobs it holds, hands whatever is
still running back to other instances at once, and closes. After a signal the
process exits once nothing holds it open, or 5 s after the stop at the latest;
a second signal ends it at once.

A supervisor or embedded host can trigger the same lifecycle with
`service.stop()`. If your application starts additional timers, database
pools, or background tasks, close them after `wait()` returns. Trellis does not
own those resources.

For tests, close clients and stop the service in `finally`; always stop the test
runtime even when an assertion fails. The
[checked-in test](https://github.com/oats-center/trellis/blob/main/docs/examples/orders/service_test.ts)
shows this sequence.

Rust uses its generated service `run()` lifecycle rather than TypeScript's
`wait()` API; it handles the same signals the same way and returns `Ok(())`.
Follow the [Rust library guide](/guides/libraries/rust) for the generated
facade. Application-created tasks outside that lifecycle need their own
shutdown handling.

To replace a running service with a new build without dropping requests, see
[Upgrade a service without downtime](/guides/upgrade-without-downtime).
