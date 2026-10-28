import {
  type PermissionAtom as DescriptorPermissionAtom,
  type RuntimeApi,
} from "../participant_runtime/api.ts";
import {
  EventVerificationAuthError,
  type RuntimeRpcErrorDesc,
  type VerifiedCaller,
} from "./local_authorization.ts";
import { TrellisTasks } from "../tasks.ts";
import { type LoggerLike } from "../globals.ts";
import {
  type TransportLease,
  type TrellisTransportProvider,
} from "../transport/generations.ts";
import {
  headers as natsHeaders,
  type Msg,
  type NatsConnection,
} from "@nats-io/nats-core";
import {
  jetstream,
  jetstreamManager,
  type ConsumerMessages,
  type JetStreamClient,
} from "@nats-io/jetstream";
import { type EventOpts, type RequestOpts } from "./types.ts";
import {
  AsyncResult,
  BaseError,
  Result,
  err,
  isErr,
  ok,
} from "@oatscenter/result";
import {
  type EventDescriptorOf,
  type EventOf,
  type EventsOf,
} from "./contract_types.ts";
import { natsSubjectMatches, type EventCallback } from "./facades.ts";
import {
  UnexpectedError,
  ValidationError,
  type SchemaValidationError,
} from "../errors/index.ts";
import { type OriginalEventPublication } from "../auth/protocol_wasm.ts";
import {
  eventConsumerGroupEvents,
  isConsumerNotFoundError,
  jetStreamDeliveryProof,
  type ConsumerReplayEnvelope,
  type DurableEventConsumerLoop,
  type DurableEventRegistration,
  type RuntimeEventConsumers,
} from "./events.ts";
import { ulid } from "ulid";
import { sleep } from "./helpers.ts";

import { recordRuntimeError } from "./handler_errors.ts";
import { recordCatalogCounter } from "../telemetry/mod.ts";
import { API as EVENTS_API } from "../internal_sdk/generated/apis/events/mod.js";
import { permission as actionPermission } from "../participant_runtime/participant.ts";

/** Durable event consumption for a Trellis instance: consumer bindings,
 * their worker loops, delivery reporting, and consumer management. */

/** The descriptor fields of a built-in RPC request. */
export type BuiltRpcContext = {
  subject: string;
  permission: DescriptorPermissionAtom;
  input: unknown;
  output: unknown;
  callerCapabilities: readonly string[];
  errors?: readonly string[];
  declaredErrorTypes?: readonly string[];
  runtimeErrors?: readonly RuntimeRpcErrorDesc[];
};

/** The parts of its Trellis instance durable event consumption uses. */
export type DurableEventHost<TA extends RuntimeApi> = {
  readonly timeout: number;
  readonly tasks: TrellisTasks;
  readonly log: LoggerLike;
  readonly adaptiveTransport: boolean;
  transport(): TrellisTransportProvider;
  nats(): NatsConnection;
  js(): JetStreamClient;
  requestBuiltRpc<TOutput>(
    method: string,
    input: unknown,
    ctx: BuiltRpcContext,
    opts?: RequestOpts,
  ): AsyncResult<TOutput, BaseError>;
  invokeEventHandler(args: {
    event: EventsOf<TA>;
    payload: unknown;
    mode: "durable" | "ephemeral";
    group?: string;
    message: Pick<Msg, "headers" | "subject"> & object;
    fn: EventCallback<EventOf<TA, EventsOf<TA>>>;
  }): Promise<Result<void, BaseError>>;
  parseEventMessage(
    event: EventsOf<TA>,
    ctx: EventDescriptorOf<TA, EventsOf<TA>>,
    msg: Pick<Msg, "json" | "subject">,
  ): Result<unknown, SchemaValidationError | ValidationError | UnexpectedError>;
  validateEventProof(
    event: EventsOf<TA>,
    descriptor: EventDescriptorOf<TA, EventsOf<TA>>,
    msg: Pick<Msg, "data" | "headers" | "subject">,
    originalPublication?: OriginalEventPublication,
  ): Promise<Result<VerifiedCaller, BaseError>>;
};

export class DurableEventConsumers<TA extends RuntimeApi> {
  readonly #host: DurableEventHost<TA>;
  readonly #consumers: RuntimeEventConsumers;
  #loops = new Map<string, DurableEventConsumerLoop<TA>>();
  #stopped = false;

  constructor(host: DurableEventHost<TA>, consumers: RuntimeEventConsumers) {
    this.#host = host;
    this.#consumers = consumers;
  }

  /** Stops durable event listener loops without closing the underlying transport. */
  stop(): void {
    this.#stopped = true;
    for (const loop of this.#loops.values()) {
      loop.registrations.splice(0, loop.registrations.length);
      for (const messages of loop.messages) messages.stop();
    }
  }

  resolveGroup(
    event: EventsOf<TA>,
    opts: EventOpts | undefined,
  ): Result<string, UnexpectedError> {
    const metadata = this.#consumers.metadata;
    const bindings = this.#consumers.bindings ?? {};
    const groups = Object.entries(metadata ?? {})
      .filter(([, group]) =>
        eventConsumerGroupEvents(group).includes(String(event)),
      )
      .map(([group]) => group);

    if (opts?.group) {
      if (!groups.includes(opts.group)) {
        return err(
          new UnexpectedError({
            cause: new Error(
              `Event '${String(
                event,
              )}' is not declared in event consumer group '${opts.group}'.`,
            ),
            context: { event: String(event), group: opts.group },
          }),
        );
      }
      if (!bindings[opts.group]) {
        return err(
          new UnexpectedError({
            cause: new Error(
              `Event consumer group '${opts.group}' has no Trellis-provisioned binding.`,
            ),
            context: { event: String(event), group: opts.group },
          }),
        );
      }
      return ok(opts.group);
    }

    if (groups.length === 0) {
      return err(
        new UnexpectedError({
          cause: new Error(
            `Event '${String(
              event,
            )}' is not declared in any event consumer group.`,
          ),
          context: { event: String(event) },
        }),
      );
    }
    if (groups.length > 1) {
      return err(
        new UnexpectedError({
          cause: new Error(
            `Event '${String(
              event,
            )}' is declared in multiple event consumer groups; pass opts.group.`,
          ),
          context: { event: String(event), groups },
        }),
      );
    }

    const group = groups[0]!;
    if (!bindings[group]) {
      return err(
        new UnexpectedError({
          cause: new Error(
            `Event consumer group '${group}' has no Trellis-provisioned binding.`,
          ),
          context: { event: String(event), group },
        }),
      );
    }
    return ok(group);
  }

  register(args: {
    group: string;
    event: EventsOf<TA>;
    ctx: EventDescriptorOf<TA, EventsOf<TA>>;
    subject: string;
    fn: EventCallback<EventOf<TA, EventsOf<TA>>>;
    signal?: AbortSignal;
  }): void {
    if (args.signal?.aborted || this.#stopped) return;

    const binding = this.#consumers.bindings?.[args.group];
    if (!binding) {
      throw new Error(
        `Event consumer group '${args.group}' has no Trellis-provisioned binding.`,
      );
    }
    if (!Number.isInteger(binding.concurrency) || binding.concurrency < 1) {
      throw new Error(
        `Event consumer group '${args.group}' has invalid concurrency ${binding.concurrency}; expected a positive integer`,
      );
    }

    const loop = this.#loops.get(args.group) ?? {
      registrations: [],
      concurrency: binding.concurrency,
      startedWorkers: new Set<number>(),
      messages: new Set<ConsumerMessages>(),
    };
    if (loop.concurrency !== binding.concurrency) {
      throw new Error(
        `Event consumer group '${args.group}' is already registered with concurrency ${loop.concurrency}; binding now requires ${binding.concurrency}`,
      );
    }
    const registration: DurableEventRegistration<TA> = {
      event: args.event,
      ctx: args.ctx,
      subject: args.subject,
      fn: args.fn,
    };
    loop.registrations.push(registration);
    this.#loops.set(args.group, loop);

    args.signal?.addEventListener(
      "abort",
      () => {
        const index = loop.registrations.indexOf(registration);
        if (index >= 0) loop.registrations.splice(index, 1);
        if (!this.#durableEventConsumerGroupReady(args.group, loop)) {
          for (const messages of loop.messages) messages.stop();
        }
      },
      { once: true },
    );

    this.#startDurableEventConsumer(args.group, loop);
  }

  #startDurableEventConsumer(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
  ): void {
    if (this.#stopped || !this.#durableEventConsumerGroupReady(group, loop)) {
      return;
    }
    for (
      let workerIndex = 0;
      workerIndex < loop.concurrency;
      workerIndex += 1
    ) {
      if (loop.startedWorkers.has(workerIndex)) continue;
      loop.startedWorkers.add(workerIndex);
      this.#host.tasks.add(
        `event-consumer:${group}:${workerIndex}:${ulid()}`,
        this.#runDurableEventConsumer(group, loop, workerIndex),
      );
    }
  }

  #durableEventConsumerGroupReady(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
  ): boolean {
    const metadata = this.#consumers.metadata?.[group];
    if (!metadata) return false;
    return eventConsumerGroupEvents(metadata).every((event) =>
      loop.registrations.some(
        (registration) => String(registration.event) === event,
      ),
    );
  }

  /**
   * Runs work bound to the current ready generation's JetStream client, holding
   * that generation's lease for the duration. A fixed connection uses the
   * initial socket.
   *
   * Durable event consumption re-acquires here on every bounded batch, so a
   * growth rollover is adopted on the next fetch while a delivery already being
   * handled keeps the generation that accepted it pinned through its ack.
   */
  async #withDurableEventGeneration<U>(
    work: (deps: {
      nc: NatsConnection;
      js: JetStreamClient;
      jsm: Awaited<ReturnType<typeof jetstreamManager>>;
      lease?: TransportLease;
    }) => Promise<U>,
  ): Promise<U> {
    let lease: TransportLease | undefined;
    if (this.#host.adaptiveTransport) {
      lease = await this.#host.transport().acquireCurrent({
        deadlineMs: Date.now() + this.#host.timeout,
      });
    }
    const nc = lease?.nc ?? this.#host.nats();
    try {
      const js = lease ? jetstream(nc) : this.#host.js();
      const jsm = lease
        ? await jetstreamManager(nc)
        : await jetstreamManager(this.#host.nats());
      return await work({ nc, js, jsm, ...(lease ? { lease } : {}) });
    } finally {
      lease?.release();
    }
  }

  #runDurableEventConsumer(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
    workerIndex: number,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    return AsyncResult.from(
      (async () => {
        const binding = this.#consumers.bindings?.[group];
        if (!binding) {
          return err(
            new UnexpectedError({
              cause: new Error(
                `Event consumer group '${group}' has no Trellis-provisioned binding.`,
              ),
              context: { group },
            }),
          );
        }

        let originalOpened = false;
        let fetchingReplay = true;
        try {
          const infoResult = await AsyncResult.try(async () => {
            return await this.#withDurableEventGeneration(({ jsm }) =>
              jsm.consumers.info(binding.stream, binding.consumerName),
            );
          });
          const info = infoResult.take();
          if (isErr(info)) {
            if (
              this.#stopped ||
              !this.#durableEventConsumerGroupReady(group, loop)
            ) {
              return ok(undefined);
            }
            if (isConsumerNotFoundError(info.error.cause)) {
              this.#host.log.debug(
                {
                  group,
                  stream: binding.stream,
                  consumer: binding.consumerName,
                },
                "Durable event consumer is not available yet; retrying",
              );
              await sleep(25);
              return ok(undefined);
            }
            return info;
          }
          originalOpened = true;

          // Consumer metadata is stable across generations; only the physical
          // connection it is consumed on moves.
          const replayInfo = await this.#withDurableEventGeneration(({ jsm }) =>
            jsm.consumers.info(
              binding.replayBinding.stream,
              binding.replayBinding.consumerName,
            ),
          );
          let replay = false;
          while (
            !this.#stopped &&
            this.#durableEventConsumerGroupReady(group, loop)
          ) {
            fetchingReplay = replay;
            const isReplay = replay;
            replay = !replay;
            // One bounded batch per fetch on the current ready generation, pinned
            // through the handler and its ack. Idle iterations release promptly,
            // so growth is adopted on the next fetch with no second logical
            // consumer.
            await this.#withDurableEventGeneration(async ({ js }) => {
              const messages = await js.consumers
                .getConsumerFromInfo(isReplay ? replayInfo : info)
                .fetch({ max_messages: 1, expires: 1_000 });
              loop.messages.add(messages);
              if (!this.#durableEventConsumerGroupReady(group, loop)) {
                messages.stop();
                loop.messages.delete(messages);
                return;
              }
              try {
                await this.#handleDurableEventConsumer(
                  group,
                  loop,
                  messages,
                  isReplay,
                ).orThrow();
              } finally {
                loop.messages.delete(messages);
              }
            });
          }
        } catch (cause) {
          if (
            this.#stopped ||
            !this.#durableEventConsumerGroupReady(group, loop)
          ) {
            return ok(undefined);
          }
          if (
            this.#host.adaptiveTransport &&
            this.#host.transport().isClosed()
          ) {
            // Terminal: a closed or revoked logical transport must stop durable
            // consumption rather than retrying forever against no generation.
            this.#stopped = true;
            return ok(undefined);
          }
          if (
            isConsumerNotFoundError(cause) &&
            (fetchingReplay || !originalOpened)
          ) {
            this.#host.log.debug(
              { group, stream: binding.stream, consumer: binding.consumerName },
              "Durable event consumer is not available yet; retrying",
            );
            await sleep(25);
            return ok(undefined);
          }
          await sleep(25);
          return err(new UnexpectedError({ cause, context: { group } }));
        } finally {
          loop.startedWorkers.delete(workerIndex);
          if (!this.#stopped) {
            this.#startDurableEventConsumer(group, loop);
          }
        }
        return ok(undefined);
      })(),
    );
  }

  #handleDurableEventConsumer(
    group: string,
    loop: DurableEventConsumerLoop<TA>,
    messages: ConsumerMessages,
    isReplay = false,
  ): AsyncResult<void, ValidationError | UnexpectedError> {
    return AsyncResult.try(async () => {
      const binding = this.#consumers.bindings?.[group];
      if (!binding) {
        throw new Error(`Event consumer group '${group}' is unavailable`);
      }
      for await (const msg of messages) {
        let replayEnvelope: ConsumerReplayEnvelope | undefined;
        let originalPublication: OriginalEventPublication = {
          publishedAt: new Date(
            msg.info.timestampNanos / 1_000_000,
          ).toISOString(),
          streamSequence: String(msg.info.streamSequence),
        };
        let deliveredMessage: Pick<
          Msg,
          "data" | "headers" | "subject" | "json"
        > = msg;
        if (isReplay) {
          const parsedReplayEnvelope = JSON.parse(
            new TextDecoder().decode(msg.data),
          ) as ConsumerReplayEnvelope;
          replayEnvelope = parsedReplayEnvelope;
          if (
            parsedReplayEnvelope.resourceId !== binding.resourceId ||
            parsedReplayEnvelope.originalRecordSequence <= 0
          ) {
            this.#recordEventDisposition("nak", () => msg.nak());
            continue;
          }
          let inspected: Record<string, unknown>;
          try {
            inspected = await this.#requestConsumerManagement(
              "DeadLetters.Inspect",
              {
                resourceId: binding.resourceId,
                deadLetterId: parsedReplayEnvelope.deadLetterId,
              },
            );
          } catch (error) {
            this.#host.log.warn(
              { error, group },
              "Replay state inspection unavailable",
            );
            const delivery = msg.info.deliveryCount;
            this.#recordEventDisposition("nak", () =>
              msg.nak(
                binding.backoffMs[
                  Math.min(
                    Math.max(0, delivery - 1),
                    binding.backoffMs.length - 1,
                  )
                ] ?? binding.ackWaitMs,
              ),
            );
            continue;
          }
          const detail = Reflect.get(inspected, "deadLetter");
          const deadLetter =
            detail && typeof detail === "object"
              ? Reflect.get(detail, "deadLetter")
              : undefined;
          const projectedGeneration =
            deadLetter && typeof deadLetter === "object"
              ? Reflect.get(deadLetter, "generation")
              : undefined;
          if (
            typeof projectedGeneration !== "number" ||
            projectedGeneration < parsedReplayEnvelope.generation
          ) {
            this.#recordEventDisposition("nak", () => msg.nak(25));
            continue;
          }
          if (
            projectedGeneration !== parsedReplayEnvelope.generation ||
            !["replayPending", "replaying"].includes(
              Reflect.get(deadLetter, "state"),
            )
          ) {
            this.#recordEventDisposition("ack", () => msg.ack());
            continue;
          }
          if (!detail || typeof detail !== "object") {
            this.#recordEventDisposition("nak", () =>
              msg.nak(binding.ackWaitMs),
            );
            continue;
          }
          const publishedAt = Reflect.get(detail, "originalPublishedAt");
          const sequence = Reflect.get(deadLetter, "originalSequence");
          const data = Reflect.get(detail, "originalPayload");
          const subject = Reflect.get(detail, "originalSubject");
          const headers = Reflect.get(detail, "originalHeaders");
          if (
            typeof publishedAt !== "string" ||
            !Number.isFinite(Date.parse(publishedAt)) ||
            typeof sequence !== "bigint" ||
            !(data instanceof Uint8Array) ||
            typeof subject !== "string" ||
            !headers ||
            typeof headers !== "object"
          ) {
            this.#recordEventDisposition("nak", () =>
              msg.nak(binding.ackWaitMs),
            );
            continue;
          }
          originalPublication = {
            publishedAt,
            streamSequence: String(sequence),
          };
          // Use the authenticated inspection's retained bytes with its broker time,
          // never publisher-supplied replay content with somebody else's provenance.
          const originalHeaders = natsHeaders();
          for (const [name, value] of Object.entries(headers)) {
            if (typeof value === "string") originalHeaders.append(name, value);
          }
          deliveredMessage = {
            data,
            headers: originalHeaders,
            subject,
            json: () => JSON.parse(new TextDecoder().decode(data)),
          };
        }
        if (!this.#durableEventConsumerGroupReady(group, loop)) {
          messages.stop();
          break;
        }
        const matching = loop.registrations.filter((registration) =>
          natsSubjectMatches(registration.subject, deliveredMessage.subject),
        );
        if (matching.length === 0) {
          this.#host.log.warn(
            { group, subject: msg.subject },
            "Durable event consumer received message without registered handler",
          );
          this.#recordEventDisposition("nak", () => msg.nak());
          continue;
        }

        let failed = false;
        for (const registration of matching) {
          const proofResult = await this.#host.validateEventProof(
            registration.event,
            registration.ctx,
            deliveredMessage,
            originalPublication,
          );
          const proofValue = proofResult.take();
          if (isErr(proofValue)) {
            recordRuntimeError(proofValue.error, {
              surface: "event",
              direction: "consumer",
              operation: String(registration.event),
              phase: "auth",
            });
            this.#host.log.warn(
              {
                error: proofValue.error,
                event: registration.event,
                subject: msg.subject,
              },
              "Event auth validation failed",
            );
            if (
              proofValue.error instanceof EventVerificationAuthError &&
              proofValue.error.retryable
            ) {
              const delivery = msg.info.deliveryCount;
              this.#recordEventDisposition("nak", () =>
                msg.nak(
                  binding.backoffMs[
                    Math.min(
                      Math.max(0, delivery - 1),
                      binding.backoffMs.length - 1,
                    )
                  ] ?? binding.ackWaitMs,
                ),
              );
            } else {
              if (replayEnvelope) {
                try {
                  msg.working();
                  await this.#reportConsumerDelivery({
                    resourceId: binding.resourceId,
                    sourceStream: binding.replayBinding.stream,
                    sourceSequence: BigInt(msg.info.streamSequence),
                    deliveryCount: msg.info.deliveryCount,
                    deliveryProof: jetStreamDeliveryProof(msg),
                    replayGeneration: replayEnvelope.generation,
                    outcome: "unreplayable",
                    error: proofValue.error.message,
                  });
                } catch (error) {
                  this.#host.log.warn(
                    { error, group },
                    "Replay delivery report unavailable",
                  );
                  failed = true;
                  break;
                }
              }
              this.#recordEventDisposition("term", () => msg.term());
            }
            failed = true;
            break;
          }

          const parsedEvent = this.#host.parseEventMessage(
            registration.event,
            registration.ctx,
            deliveredMessage,
          );
          const eventPayload = parsedEvent.take();
          if (isErr(eventPayload)) {
            this.#host.log.error(
              { error: eventPayload.error },
              "Event validation failed",
            );
            recordRuntimeError(eventPayload.error, {
              surface: "event",
              direction: "consumer",
              operation: String(registration.event),
              phase: "input_validation",
            });
            if (replayEnvelope) {
              try {
                await this.#reportConsumerDelivery({
                  resourceId: binding.resourceId,
                  sourceStream: binding.replayBinding.stream,
                  sourceSequence: BigInt(msg.info.streamSequence),
                  deliveryCount: msg.info.deliveryCount,
                  deliveryProof: jetStreamDeliveryProof(msg),
                  replayGeneration: replayEnvelope.generation,
                  outcome: "unreplayable",
                  error: eventPayload.error.message,
                });
              } catch (error) {
                this.#host.log.warn(
                  { error, group },
                  "Replay terminal report unavailable",
                );
                failed = true;
                break;
              }
            }
            this.#recordEventDisposition("term", () => msg.term());
            failed = true;
            break;
          }

          const delivery = msg.info.deliveryCount;
          const wait =
            binding.backoffMs[
              Math.min(Math.max(0, delivery - 1), binding.backoffMs.length - 1)
            ] ?? binding.ackWaitMs;
          const progress = setInterval(
            () => msg.working(),
            Math.max(1, Math.min(wait - 1, Math.floor(wait / 3))),
          );
          const handlerResult = await this.#host
            .invokeEventHandler({
              event: registration.event,
              payload: eventPayload,
              mode: "durable",
              group,
              message: deliveredMessage,
              fn: registration.fn,
            })
            .finally(() => clearInterval(progress));
          const handlerValue = handlerResult.take();
          if (isErr(handlerValue)) {
            recordRuntimeError(handlerValue.error, {
              surface: "event",
              direction: "consumer",
              operation: String(registration.event),
              phase: "handler_result",
            });
            this.#host.log.error(
              {
                error: handlerValue.error.toSerializable(),
                event: registration.event,
                subject: msg.subject,
              },
              "Event handler failed",
            );
            if (delivery >= binding.maxDeliver) {
              // The broker's exhaustion advisory owns DLQ capture, including replay.
              this.#recordEventDisposition("nak", () => msg.nak());
              failed = true;
              break;
            }
            const delay =
              binding.backoffMs[
                Math.min(
                  Math.max(0, delivery - 1),
                  binding.backoffMs.length - 1,
                )
              ] ?? 0;
            this.#recordEventDisposition("nak", () => msg.nak(delay));
            failed = true;
            break;
          }
        }

        if (!failed) {
          if (replayEnvelope) {
            try {
              msg.working();
              await this.#reportConsumerDelivery({
                resourceId: binding.resourceId,
                sourceStream: binding.replayBinding.stream,
                sourceSequence: BigInt(msg.info.streamSequence),
                deliveryCount: msg.info.deliveryCount,
                deliveryProof: jetStreamDeliveryProof(msg),
                replayGeneration: replayEnvelope.generation,
                outcome: "succeeded",
              });
            } catch (error) {
              this.#host.log.warn(
                { error, group },
                "Delivery report unavailable",
              );
              continue;
            }
          }
          this.#recordEventDisposition("ack", () => msg.ack());
        }
      }
    });
  }

  async #reportConsumerDelivery(input: Record<string, unknown>): Promise<void> {
    let outcome = "error";
    try {
      await this.#requestConsumerManagement("Consumers.ReportDelivery", input);
      outcome = "ok";
    } finally {
      try {
        recordCatalogCounter("trellis.delivery.dispositions", 1, {
          "trellis.family": "event",
          "trellis.action":
            input.outcome === "exhausted"
              ? "exhausted_report"
              : "replay_report",
          "trellis.outcome": outcome,
        });
      } catch {
        // The report's original success or error owns the business outcome.
      }
    }
  }

  #recordEventDisposition(
    action: "ack" | "nak" | "term",
    publish: () => void,
  ): void {
    let outcome = "error";
    try {
      publish();
      outcome = "ok";
    } finally {
      try {
        recordCatalogCounter("trellis.delivery.dispositions", 1, {
          "trellis.family": "event",
          "trellis.action": action,
          "trellis.outcome": outcome,
        });
      } catch {
        // A failed optional observation must not replace a broker error.
      }
    }
  }

  async #requestConsumerManagement(
    method: string,
    input: Record<string, unknown>,
  ): Promise<Record<string, unknown>> {
    const action =
      method === "Consumers.ReportDelivery"
        ? EVENTS_API.actions["rpc:Consumers.ReportDelivery"]
        : method === "DeadLetters.Inspect"
          ? EVENTS_API.actions["rpc:DeadLetters.Inspect"]
          : undefined;
    if (!action) {
      throw new Error(`Consumer management '${method}' is unavailable`);
    }
    return await this.#host
      .requestBuiltRpc<Record<string, unknown>>(method, input, {
        subject: action.subject,
        permission: actionPermission(EVENTS_API.identity, action, "call"),
        input: action.input,
        output: action.output,
        callerCapabilities: [],
        errors: action.errors.map((error) => error.type),
        declaredErrorTypes: action.errors.map((error) => error.type),
        runtimeErrors: action.errors,
      })
      .orThrow();
  }
}
