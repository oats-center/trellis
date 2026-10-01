/**
 * Real-boundary acceptance for the flat provider handler client.
 *
 * Case A drives one Provider job through its injected client: availability,
 * transient updates, an outbound operation invocation, KV writes, a prepared
 * owned Event, and a Live handler that reaches back through the same client.
 * Case B proves a selected external Event subscription receives a flat client
 * whose selected publisher and prepared publish reach the real transport.
 */

import { assert, assertEquals } from "@std/assert";
import { TrellisService } from "@oatscenter/trellis/service";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import {
  changedListener,
  echoHandler,
  exerciseHandlerClient,
  operationRegistration,
  watchHandler,
  workHandler,
  workJobHandler,
} from "./_support/provider_handler_consumer.ts";
import { withTrellisRuntime } from "./_support/runtime.ts";

Deno.test(
  "provider handler client works across job, live, event, resources and prepared publish",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const name = `provider-handler-${crypto.randomUUID()}`;
      const identity = await runtime.registerService({
        name,
        contract: participants.Provider.participant,
      });
      const service = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.Provider.participant,
        name,
        seed: identity.seed,
      }).orThrow();
      let serviceExit: Promise<unknown> | undefined;
      const failures: unknown[] = [];
      try {
        await service.handleEcho(echoHandler);
        await operationRegistration(service)(workHandler);
        await service.handleWatch(watchHandler);
        await service.onChanged(changedListener);
        service.jobs.work.handle(workJobHandler);
        serviceExit = service.wait().catch((error: unknown) => {
          failures.push(error);
        });

        assertEquals(
          await exerciseHandlerClient(service, "consumer"),
          "echo:consumer",
        );

        const job = await service.jobs.work.create({ value: "live-job" })
          .orThrow();
        const updates = await job.updates().orThrow();
        const iterator = updates[Symbol.asyncIterator]();
        let receivedUpdate:
          | Awaited<ReturnType<typeof iterator.next>>
          | undefined;
        let updateError: unknown;
        const nextUpdate = iterator.next().then(
          (update) => {
            receivedUpdate = update;
          },
          (error: unknown) => {
            updateError = error;
          },
        );
        try {
          await service.kv.records.put(`release-${job.id}`, { value: "ready" })
            .orThrow();
          const terminal = await job.wait().orThrow();
          assertEquals(terminal.state, "completed", terminal.lastError);
          assertEquals(terminal.result, { value: "work:live-job" });
          const received = await runtime.waitFor(() => {
            if (updateError !== undefined) throw updateError;
            return receivedUpdate ?? false;
          });
          assert(!received.done);
          assertEquals(received.value, {
            value: "live-job",
            nested: {
              count: 9_007_199_254_740_993n,
              payload: new Uint8Array([0, 128, 255]),
            },
          });
          assertEquals(typeof received.value.nested.count, "bigint");
          assert(received.value.nested.payload instanceof Uint8Array);
        } finally {
          updates.unsubscribe();
          await nextUpdate;
          await iterator.return?.();
        }

        const records = service.kv.records;
        assert(records, "the required KV records binding must be available");
        assertEquals(
          await records.get("job").orThrow(),
          { value: "work:live-job" },
          "the job handler persisted the operation output through its client",
        );
        const observedEvent = await runtime.waitFor(async () => {
          const event = await records.get("event").orThrow();
          return event?.value === "work:live-job" ? event : false;
        });
        assertEquals(
          observedEvent,
          { value: "work:live-job" },
          "the owned Changed listener persisted the prepared event through its client",
        );

        const client = await runtime.connectClient({
          name: `provider-handler-caller-${crypto.randomUUID()}`,
          contract: participants.Caller.participant,
        });
        try {
          const live = await client.watch({}).orThrow();
          const frames: string[] = [];
          for await (const frame of live) {
            frames.push(frame.value);
          }
          assertEquals(
            frames,
            ["echo:live"],
            "the Live handler reached the flat client's selected Echo call",
          );
        } finally {
          await client.connection.close().catch((error: unknown) => {
            failures.push(error);
          });
        }
      } catch (error) {
        failures.unshift(error);
      } finally {
        await service.stop().catch((error: unknown) => {
          failures.push(error);
        });
        await serviceExit;
      }
      if (failures.length) {
        throw new AggregateError(failures, "Provider case failed");
      }
    });
  },
);

Deno.test(
  "selected external event subscription receives a flat provider client",
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const serviceName = `event-service-${crypto.randomUUID()}`;
      const serviceIdentity = await runtime.registerService({
        name: serviceName,
        contract: participants.EventService.participant,
      });
      const eventService = await TrellisService.connect({
        trellisUrl: runtime.trellisUrl,
        participant: participants.EventService.participant,
        name: serviceName,
        seed: serviceIdentity.seed,
      }).orThrow();
      let serviceExit: Promise<unknown> | undefined;
      const failures: unknown[] = [];
      try {
        const received: string[] = [];
        const clientErrors: string[] = [];
        let betaDeliveries = 0;
        await eventService.onAlpha(({ event }) => {
          received.push(event.value);
        }).orThrow();
        // The declared durable consumer spans Alpha and Beta, so its listener
        // loop only starts once every declared event has a registration.
        await eventService.onBeta(() => {}).orThrow();
        serviceExit = eventService.wait().catch((error: unknown) => {
          failures.push(error);
        });

        const clientName = `event-client-${crypto.randomUUID()}`;
        const clientIdentity = await runtime.registerService({
          name: clientName,
          contract: participants.EventClient.participant,
          deployment: `event-client-deployment-${crypto.randomUUID()}`,
        });
        const eventClient = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.EventClient.participant,
          name: clientName,
          seed: clientIdentity.seed,
        }).orThrow();
        let clientExit: Promise<unknown> | undefined;
        try {
          await eventClient.onBeta(
            async ({ event, client }) => {
              betaDeliveries += 1;
              try {
                const prepared = client.publishAlpha.prepare({
                  site: event.site,
                  value: `alpha:${event.value}`,
                }).orThrow();
                await client.publishPrepared(prepared).orThrow();
              } catch (error) {
                clientErrors.push(
                  error instanceof Error ? error.message : String(error),
                );
                throw error;
              }
            },
            {},
            { mode: "ephemeral" },
          ).orThrow();
          clientExit = eventClient.wait().catch((error: unknown) => {
            failures.push(error);
          });

          await eventService.publishBeta({ site: "case-b", value: "beta-one" })
            .orThrow();
          await runtime.waitFor(
            () => received.includes("alpha:beta-one"),
            { timeoutMs: 20_000 },
          );
          assertEquals(clientErrors, []);
          assert(
            received.includes("alpha:beta-one"),
            `the owned listener must receive the transformed Alpha (beta deliveries: ${betaDeliveries})`,
          );
          assert(betaDeliveries >= 1);
        } catch (error) {
          failures.unshift(error);
        } finally {
          await eventClient.stop().catch((error: unknown) => {
            failures.push(error);
          });
          await clientExit;
        }
      } catch (error) {
        failures.unshift(error);
      } finally {
        await eventService.stop().catch((error: unknown) => {
          failures.push(error);
        });
        await serviceExit;
      }
      if (failures.length) {
        throw new AggregateError(failures, "Event case failed");
      }
    });
  },
);
