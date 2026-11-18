/** A lost connection is replaced by a newly admitted generation, never reattached in place. */
import assert from "node:assert/strict";
import { test } from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { Result } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import {
  apis,
  apps,
  participants,
} from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "./_support/runtime.ts";

test(
  "a dropped client socket recovers on a fresh admitted generation and never delivers a request after its deadline",
  { timeout: 90_000 },
  async () => {
    await withTrellisRuntime(
      async (runtime) => {
        const identity = await runtime.registerService({
          name: "loss-provider",
          contract: participants.EchoProvider.participant,
        });
        const service = await TrellisService.connect({
          trellisUrl: runtime.trellisUrl,
          participant: participants.EchoProvider.participant,
          seed: identity.seed,
        }).orThrow();
        const caller = await runtime.connectClient({
          name: "loss-caller",
          contract: apps.EchoCaller.request,
        });
        const gate = runtime.nativeTransportGate();
        const echo = apis.runtime.API.actions["rpc:Echo"].subject;
        const executed: { value: string; at: number }[] = [];
        try {
          // No provider serves Echo yet, so the broker answers no-responders and
          // the caller resends at 0, 0.1, 0.3, 0.7, 1.5 and 2.5 s; the next
          // resend is due at 3.5 s, inside the deadline. By the sixth answer the
          // connection has outlived the NATS client's 2 s reconnect wait, so a
          // client that reconnects in place would redial at once.
          const gapDeadline = Date.now() + 4_500;
          const gap = caller.echo({ value: "gap" }, { timeout: 4_500 });
          const lost = await runtime.waitFor(
            () =>
              gate.connections().find((connection) => {
                if (connection.closed) return false;
                if (
                  connection.outboundContexts.filter(
                    (message) => message.subject === echo,
                  ).length < 6
                )
                  return false;
                const replies = new Map<string, number>();
                for (const delivery of connection.deliveries)
                  replies.set(
                    delivery.subject,
                    (replies.get(delivery.subject) ?? 0) + 1,
                  );
                return Math.max(...replies.values()) >= 6;
              }),
            { timeoutMs: 10_000, intervalMs: 10 },
          );
          assert(
            lost.subs.some(
              (subscription) => !subscription.subject.startsWith("_INBOX."),
            ),
            "the caller's connection holds a subscription beyond its inboxes",
          );
          // The next connection stalls in its handshake, so recovery cannot
          // finish before the deadlines below pass. The broker stays reachable.
          const stall = gate.holdNextPong();
          runtime.dropNativeConnection(lost.id);
          await service.handleEcho(({ input }) => {
            executed.push({ value: input.value, at: Date.now() });
            return Result.ok(input);
          });
          const expiringDeadline = Date.now() + 1_500;
          const expiring = caller.echo(
            { value: "expiring" },
            { timeout: 1_500 },
          );
          const waiting = caller.echo(
            { value: "waiting" },
            { timeout: 30_000 },
          );
          await stall.held;
          assert((await gap).isErr());
          assert((await expiring).isErr());
          // Recovery resumes only once every deadline above has passed.
          await delay(
            Math.max(gapDeadline, expiringDeadline) + 100 - Date.now(),
          );
          const replacement = gate
            .connections()
            .find((connection) => connection.id > lost.id);
          assert(replacement, "no connection followed the lost one");
          // Withhold the replacement's admission answer and read what it has
          // subscribed meanwhile: a fresh generation subscribes only its inbox
          // until admitted, while a reattached connection resumes everything.
          const admission = gate.armResponseHold(
            "trellis.auth.admission.",
            replacement.id,
          );
          await stall.release();
          const admitted = await Promise.race([
            admission.held.then(() => true),
            gate.waitForClose(replacement.id).then(() => false),
          ]);
          const resumed = (gate.connection(replacement.id)?.subs ?? []).filter(
            (subscription) => !subscription.subject.startsWith("_INBOX."),
          );
          await admission.release();
          assert.deepEqual(await waiting.orThrow(), { value: "waiting" });
          for (const [value, deadline] of [
            ["gap", gapDeadline],
            ["expiring", expiringDeadline],
          ] as const) {
            assert(
              !executed.some(
                (entry) => entry.value === value && entry.at >= deadline,
              ),
              `"${value}" reached the provider after its deadline`,
            );
          }
          assert.deepEqual(
            executed.map((entry) => entry.value),
            ["waiting"],
          );
          assert(admitted, "the connection after the loss was never admitted");
          assert.deepEqual(resumed, []);
        } finally {
          await caller.connection.close();
          await service.stop();
        }
      },
      { interruptibleNativeProxy: true },
    );
  },
);
