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
> = ({ input }) => Result.ok({ orderId: ulid(), customerId: input.customerId });
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

await service.handleCreate(createOrder);
console.log("Orders service connected; press Ctrl-C to stop.");
// SIGTERM and Ctrl-C stop it gracefully: it finishes the work it holds first.
await service.wait();
```

Environment variables and their names are this example's configuration
interface, not a Trellis requirement. Pass the same typed options from a file,
secret manager, CLI parser, or application configuration system if preferred.
The service's own settings, such as a database URL or password, can instead be
declared in its IDL and kept by Trellis; see
[Settings and secrets](/guides/settings-and-secrets).

`connect()` proves the provisioned identity in NATS CONNECT, verifies the signed
session authority Auth issues for its deployment, and resolves Trellis
resources. `handleCreate()` registers the provider. `wait()` serves and supervises the
running service until SIGTERM, Ctrl-C or `stop()` stops it gracefully; see
[Handle graceful shutdown](/guides/write-a-service/shutdown). Session authority remains in
process memory and is refreshed proactively as SDK-owned maintenance. Renewal of unchanged transport authority does not replace a healthy
socket. The SDK adopts newly approved authority automatically, moving fresh
intake to a ready successor while safe accepted work finishes on its receiving
connection. Retain the service and its resource handles across these changes; no
manual transport refresh is required. `wait()` supervises the logical service,
not an individual socket. Reduced or revoked authority can interrupt affected
work. While own coverage is unavailable, fresh application traffic either waits
within its acquisition budget or reports unavailability.

The shared `/service` entrypoint runs on Node 24 or 26. For Rust, use the generated facade described in
[the Rust guide](/guides/libraries/rust), not this TypeScript options object.

## Provision and run

Log the CLI into your [running Trellis](/guides/running-trellis/development) as an
administrator. From the service project:

```sh
trellis svc orders-service create \
  --source . \
  --participant orders.OrdersService \
  --format json
```

The create command uses `orders-service` as a display name. This project also
contains a caller, so `--participant` selects the provider by its identity,
`<package>.<ServiceName>`. Create installs the participant and its deployment
resources, and accepts the API the service provides into the installation's
catalog. Set `TRELLIS_DEPLOYMENT` to the returned `deployment.deploymentId`,
not the display name. After later IDL changes, restart the service: it submits
its new build, which installs at once unless it asks for new capabilities or
resources, in which case it prints a review link for an administrator.
`trellis svc "$TRELLIS_DEPLOYMENT" apply --source . --participant orders.OrdersService`
installs a build ahead of time instead.
Inspect the deployment and provision an instance:

```sh
trellis svc "$TRELLIS_DEPLOYMENT" show --format json
trellis svc "$TRELLIS_DEPLOYMENT" provision --format json
```

Provisioning prints private identity material; retain it securely, not in source
control or logs.

| Example environment variable | Value                                                |
| ---------------------------- | ---------------------------------------------------- |
| `TRELLIS_URL`                | Trellis address, for example `http://localhost:3000` |
| `TRELLIS_DEPLOYMENT`         | Created `deployment.deploymentId`                    |
| `TRELLIS_IDENTITY_SEED`      | Provision response `instanceSeed`                    |

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
