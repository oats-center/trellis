---
title: "The service entry point"
description: "Connect the service runtime, provision an instance, and run the service locally."
---

# Connect and implement the service

Start with the generated project from
[the previous page](/guides/write-a-service/first-contract). The complete files
are in
[`docs/examples/orders`](https://github.com/oats-center/trellis/tree/main/docs/examples/orders).

## Write the handler

Create `service.ts`:

```ts
import { Result } from "@oatscenter/trellis";
import type { RpcHandler } from "@oatscenter/trellis/service";
import { participants } from "./trellis/index.js";
import { ulid } from "ulid";

/** Returns an example order receipt; this walkthrough does not persist orders. */
export const createOrder: RpcHandler<
  typeof participants.OrdersService.participant,
  "Create"
> = (
  { input },
) => Result.ok({ orderId: ulid(), customerId: input.customerId });
```

`RpcHandler` derives both request and response types from the generated
participant and exact action name. The ULID and the absence of storage are
demonstration choices. A real order service can invoke its own domain code and
database here. Trellis does not choose either.

## Connect and own the lifecycle

Replace `main.ts` with:

```ts
import { TrellisService } from "@oatscenter/trellis/service";
import { participants } from "./trellis/index.js";
import { createOrder } from "./service.ts";

function requiredEnv(name: string): string {
  const value = process.env[name];
  if (!value) throw new Error(`Missing ${name}`);
  return value;
}

const service = await TrellisService.connect({
  participant: participants.OrdersService.participant,
  name: "orders-service",
  trellisUrl: requiredEnv("TRELLIS_URL"),
  seed: requiredEnv("TRELLIS_IDENTITY_SEED"),
}).orThrow();

const stop = () => {
  void service.stop();
};
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
try {
  await service.handleCreate(createOrder);
  console.log("Orders service connected; press Ctrl-C to stop.");
  await service.wait();
} finally {
  process.off("SIGINT", stop);
  process.off("SIGTERM", stop);
  await service.stop();
}
```

Environment variables and their names are this example's configuration
interface, not a Trellis requirement. Pass the same typed options from a file,
secret manager, CLI parser, or application configuration system if preferred.

`connect()` proves the provisioned identity in NATS CONNECT, verifies the signed
session authority Auth issues for its deployment, and resolves Trellis
resources. `handleCreate()` registers the provider. `wait()` supervises the
running service; `stop()` closes its resources. Session authority remains in
process memory and is refreshed proactively as SDK-owned maintenance. Renewal of unchanged transport authority does not replace a healthy
socket. The SDK adopts newly approved authority automatically, moving fresh
intake to a ready successor while safe accepted work finishes on its receiving
connection. Retain the service and its resource handles across these changes; no
manual transport refresh is required. `wait()` supervises the logical service,
not an individual socket. Reduced or revoked authority can interrupt affected
work. While own coverage is unavailable, fresh application traffic either waits
within its acquisition budget or reports unavailability.

The shared `/service` entrypoint runs on Node 24. For Rust, use the generated facade described in
[the Rust guide](/guides/libraries/rust), not this TypeScript options object.

## Provision and run

Log the CLI into your [running Trellis](/guides/starting-trellis) as an
administrator. From the service project:

```sh
trellis svc orders-service create --format json
```

The create command uses `orders-service` as a display name. Set
`TRELLIS_DEPLOYMENT` to the returned `deployment.deploymentId`, not that display
name. This project also contains a caller; choose the provider's artifact files
explicitly:

```sh
trellis svc "$TRELLIS_DEPLOYMENT" apply \
  --source . \
  --participant acme.orders-service
```

The direct apply installs the participant and deployment resources. Inspect the
deployment and provision an instance:

```sh
trellis svc "$TRELLIS_DEPLOYMENT" show --format json
trellis svc "$TRELLIS_DEPLOYMENT" provision --format json
```

Provisioning prints private identity material; retain it securely, not in source
control or logs.

| Example environment variable | Value                                                 |
| ---------------------------- | ----------------------------------------------------- |
| `TRELLIS_URL`                | Runtime HTTP URL, for example `http://127.0.0.1:8080` |
| `TRELLIS_DEPLOYMENT`         | Created `deployment.deploymentId`                     |
| `TRELLIS_IDENTITY_SEED`      | Provision response `instanceSeed`                     |

Deployment, instance, participant, grant, resource, and transport data are
resolved by Trellis during bootstrap; do not duplicate them in service config.

Then run from the directory containing `main.ts`, with the environment
populated:

```sh
npm run check
npm start
```

Deployment permissions are an application/runtime-host choice. Restrict the
service process to the network, files, signals, and telemetry it actually uses.

Expect the connected message. Ctrl-C stops the instance. If startup waits, check
pending authority; if identity verification fails, check the provisioned
instance and exact participant binding rather than bypassing bootstrap.

To exercise the RPC without a manually provisioned deployment, continue with
[the complete live test](/guides/testing-trellis-services).
