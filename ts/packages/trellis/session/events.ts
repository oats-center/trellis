import { type EventConsumerResourceBinding } from "../participant_runtime/schemas.ts";
import { type ConsumerMessages, type JsMsg } from "@nats-io/jetstream";
import { type RuntimeApi } from "../participant_runtime/api.ts";
import { type TrellisOpts } from "./types.ts";
import {
  type EventDescriptorOf,
  type EventOf,
  type EventsOf,
} from "./contract_types.ts";
import { type EventCallback } from "./facades.ts";

/** Context provided to event listener callbacks. */
export type EventListenerContext = {
  /** Stable event id from the Trellis event header. */
  id: string;
  /** Event creation time from the Trellis event header. */
  time: Date;
  /** NATS subject that delivered the event. */
  subject: string;
  /** Runtime listener mode that delivered the event. */
  mode: "durable" | "ephemeral";
  /** Durable event consumer group, when delivered through a group. */
  group?: string;
  /** JetStream sequence number, when available. */
  sequence?: number;
};

export function createEventListenerContext(args: {
  subject: string;
  mode: "durable" | "ephemeral";
  group?: string;
  message: object;
}): EventListenerContext {
  const messageId = readMessageHeader(args.message, "Nats-Msg-Id");
  const messageTime = readMessageHeader(args.message, "Trellis-Event-Time");
  const sequence = Reflect.get(args.message, "seq");

  return {
    id: messageId ?? "",
    time: new Date(messageTime ?? 0),
    subject: args.subject,
    mode: args.mode,
    ...(args.group ? { group: args.group } : {}),
    ...(typeof sequence === "number" ? { sequence } : {}),
  };
}

export function readMessageHeader(
  message: object,
  key: string,
): string | undefined {
  const headers = Reflect.get(message, "headers");
  if (typeof headers !== "object" || headers === null) return undefined;
  const get = Reflect.get(headers, "get");
  if (typeof get !== "function") return undefined;
  const value = get.call(headers, key);
  return typeof value === "string" ? value : undefined;
}

export type RuntimeEventConsumers = {
  metadata?: RuntimeEventConsumerGroups;
  bindings?: Record<string, EventConsumerResourceBinding>;
};

export type RuntimeEventConsumerGroup = {
  uses?: Readonly<Record<string, readonly string[]>>;
  self?: readonly string[];
};

export type RuntimeEventConsumerGroups = Readonly<
  Record<string, RuntimeEventConsumerGroup>
>;

export function eventConsumerGroupEvents(
  group: RuntimeEventConsumerGroup,
): string[] {
  const events = new Set<string>();
  for (const groupEvents of Object.values(group.uses ?? {})) {
    for (const event of groupEvents) events.add(event);
  }
  for (const event of group.self ?? []) events.add(event);
  return [...events].sort();
}

export function jetStreamDeliveryProof(message: JsMsg): string {
  const raw = Reflect.get(message, "msg");
  const reply =
    raw && typeof raw === "object" ? Reflect.get(raw, "reply") : undefined;
  if (typeof reply !== "string" || !reply.startsWith("$JS.ACK.")) {
    throw new Error("JetStream delivery has no acknowledgement subject");
  }
  return reply;
}

export function isConsumerNotFoundError(error: unknown): boolean {
  return (
    error instanceof Error &&
    (error.name === "ConsumerNotFoundError" ||
      error.message.includes("consumer not found"))
  );
}

export type TrellisInternalOpts<TA extends RuntimeApi> = TrellisOpts<TA> & {
  inboxPrefix?: string;
  eventConsumers?: RuntimeEventConsumers;
  /**
   * `event:<Name>` descriptor names this participant explicitly declares as
   * subscribe needs. When present, explicit ephemeral delivery is allowed only
   * for those events; the declared durable consumer never implies it.
   */
  ephemeralEventNeeds?: ReadonlySet<string>;
};

export const internalEventConsumers = Symbol("trellis.internal.eventConsumers");

export const internalEphemeralEventNeeds = Symbol(
  "trellis.internal.ephemeralEventNeeds",
);

export type InternalizedTrellisOpts<TA extends RuntimeApi> = TrellisOpts<TA> & {
  [internalEventConsumers]?: RuntimeEventConsumers;
  [internalEphemeralEventNeeds]?: ReadonlySet<string>;
};

export type DurableEventRegistration<TA extends RuntimeApi> = {
  event: EventsOf<TA>;
  ctx: EventDescriptorOf<TA, EventsOf<TA>>;
  subject: string;
  fn: EventCallback<EventOf<TA, EventsOf<TA>>>;
};

export type DurableEventConsumerLoop<TA extends RuntimeApi> = {
  registrations: Array<DurableEventRegistration<TA>>;
  concurrency: number;
  startedWorkers: Set<number>;
  messages: Set<ConsumerMessages>;
};

export type ConsumerReplayEnvelope = {
  deadLetterId: string;
  generation: number;
  resourceId: string;
  originalRecordSequence: number;
  originalSubject: string;
  originalPayloadBytes: number[];
  originalHeaders: Record<string, string[]>;
};
