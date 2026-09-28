/**
 * Real consumer of the public flat provider handler surface.
 *
 * The exported handlers are registered by `provider_handler_client_test.ts`, so
 * the public handler aliases and the connected provider's registration
 * signatures are exercised through actual behavior, not only at compile time.
 * The remaining functions consume the same public API at compile time.
 */

import { ok } from "@oatscenter/result";
import type {
  ConnectedTrellisService,
  JobHandler,
  LiveHandler,
  OperationHandler,
  OperationRegistration,
  RpcHandler,
  ServiceEventHandler,
  ServiceHandlerClient,
} from "@oatscenter/trellis/service";

import { participants } from "../../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

/** Generated Provider contract this consumer is written against. */
export type ProviderContract = typeof participants.Provider.participant;

/** Selected flat RPC handler for the Provider's owned Echo endpoint. */
export const echoHandler: RpcHandler<ProviderContract, "Echo"> = ({ input }) =>
  ok({ value: `echo:${input.value}` });

/** Owned operation handler that completes with a transformed value. */
export const workHandler: OperationHandler<ProviderContract, "Work"> = async (
  { input, op },
) => {
  await op.started().orThrow();
  return await op.complete({ value: `work:${input.value}` }).orThrow();
};

/** Owned Live handler that reaches back through its injected client. */
export const watchHandler: LiveHandler<ProviderContract, "Watch"> = async (
  { client, emit },
) => {
  const echoed = await client.echo({ value: "live" }).orThrow();
  await emit({ value: echoed.value }).orThrow();
};

/** Owned Changed listener persisting through its injected client's KV. */
export const changedListener: ServiceEventHandler<ProviderContract, "Changed"> =
  async ({ event, client }) => {
    await client.kv.records.put("event", { value: event.value }).orThrow();
    return ok(undefined);
  };

/**
 * Owned job handler that drives the whole flat handler client: availability,
 * transient updates, outbound operation invocation, KV, and prepared publish.
 */
export const workJobHandler: JobHandler<ProviderContract, "work"> = async (
  { job, client },
) => {
  const availability = client.watchAvailability()[Symbol.asyncIterator]();
  await availability.next();
  await availability.return?.();
  await job.emitUpdate({ value: job.payload.value }).orThrow();
  const operation = await client.work({ value: job.payload.value }).start()
    .orThrow();
  const terminal = await operation.wait().orThrow();
  const completed = terminal.output;
  if (!completed) {
    throw new Error(
      `Work operation finished without output (${terminal.state})`,
    );
  }
  await client.kv.records.put("job", completed).orThrow();
  const prepared = client.publishChanged.prepare(completed).orThrow();
  await client.publishPrepared(prepared).orThrow();
  return ok(completed);
};

/** Assigns the connected provider's owned Work registration to its public type. */
export function operationRegistration(
  provider: ConnectedTrellisService<ProviderContract>,
): OperationRegistration<ProviderContract, "Work"> {
  return provider.handleWork;
}

/** Owner-fenced control and reconciliation through the public registration. */
export async function controlWork(
  registration: OperationRegistration<ProviderContract, "Work">,
  operationId: string,
): Promise<void> {
  await registration.control(operationId).orThrow();
  await registration.reconcile(operationId).orThrow();
}

/** Reads typed transient job updates for the Provider's declared update codec. */
export async function readJobUpdates(
  client: ServiceHandlerClient<ProviderContract>,
  jobId: string,
): Promise<void> {
  const updates = await client.jobs.work.updates(jobId).orThrow();
  for await (const update of updates) {
    void update.value;
  }
}

/** Publishes one prepared owned event through the canonical handler client. */
export async function publishPreparedThroughClient(
  client: ServiceHandlerClient<ProviderContract>,
  value: string,
): Promise<void> {
  const prepared = client.publishChanged.prepare({ value }).orThrow();
  await client.publishPrepared(prepared).orThrow();
}

/** Adopts a wider transport attachment through the public connection API. */
export async function refreshHandlerConnection(
  client: ServiceHandlerClient<ProviderContract>,
): Promise<void> {
  await client.connection.refreshTransport().orThrow();
}

/**
 * Exercises the canonical handler client through flat calls, availability, a
 * bound resource, and prepared publish without the registration surface.
 */
export async function exerciseHandlerClient(
  client: ServiceHandlerClient<ProviderContract>,
  value: string,
): Promise<string> {
  const echoed = await client.echo({ value }).orThrow();
  client.availability();
  for await (const availability of client.watchAvailability()) {
    void availability;
    break;
  }
  await client.kv.records.put("consumer", echoed).orThrow();
  client.publishChanged.prepare(echoed).orThrow();
  return echoed.value;
}

/** Compile-time negatives: the flat handler client is not the private session. */
export function handlerClientNegatives(
  client: ServiceHandlerClient<ProviderContract>,
): void {
  // @ts-expect-error generic invocation namespaces are not part of the flat handler client
  client.operation;
  // @ts-expect-error Provider did not select runtime.Upload for outbound use
  client.upload({ value: "not-selected" });
  // @ts-expect-error registration methods are not handler-client capabilities
  client.handleEcho;
  // @ts-expect-error lifecycle methods are not handler-client capabilities
  client.wait;
}
