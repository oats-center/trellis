import { type NatsConnection } from "@nats-io/nats-core";
import {
  type SessionAuthorityOwner,
  type VerifiedAuthority,
} from "../auth/authority.ts";
import {
  type TransportLease,
  type TrellisTransportProvider,
} from "../transport/generations.ts";
import { type RuntimeApi } from "../participant_runtime/api.ts";
import { type LoggerLike } from "../globals.ts";
import { type RuntimeStateStores } from "./contract_types.ts";
import { type ResourceMigrations } from "../kv.ts";
import { TrellisConnection } from "../connection.ts";
import { type MaybePromise } from "./facades.ts";
import { AsyncResult, BaseError } from "@oatscenter/result";
import { type LiveSubscription } from "../live/subscription.ts";
import { type SessionCaller } from "./local_authorization.ts";
import {
  UnexpectedError,
  ValidationError,
  type SchemaValidationError,
} from "../errors/index.ts";

/** One admitted generation a framework intake installs onto. @internal */
export type GenerationIntakeTarget = {
  id: number;
  nc: NatsConnection;
  contextDigest: string;
  retainAuthority?: () => VerifiedAuthority;
  lease?: () => TransportLease;
};

/** Handle returned by one framework intake install. @internal */
export type GenerationIntakeInstallResult = {
  /** Stop accepting new intake; accepted work continues to completion. */
  drain: () => Promise<void>;
  /** Resolves when every accepted intake loop has finished. */
  done: Promise<void>;
  /** Release per-generation framework resources. */
  dispose: () => Promise<void>;
};

/** Installs framework-owned broker-ready intake on one admitted generation. @internal */
export type GenerationIntakeInstall = (
  target: GenerationIntakeTarget,
) => Promise<GenerationIntakeInstallResult>;

/**
 * Handle to retire and dispose one generation's generic provider intake.
 *
 * The same logical connection can install, drain, and dispose one attachment
 * per admitted generation. `retireIntake` is idempotent and stops *new* broker
 * deliveries; `intakeStopped` resolves once every outstanding intake loop has
 * been accounted for (including queued delivery ownership), and `dispose`
 * releases per-generation resources. Disposal is invoked only after intake has
 * stopped and the generation's accepted leases have drained.
 * @internal
 */
export type ProviderGenerationAttachment = {
  retireIntake(): void;
  readonly intakeStopped: Promise<void>;
  dispose(): Promise<void>;
};

/** One installed provider ingress for one admitted generation. @internal */
export type ProviderIngress = {
  id: number;
  nc: NatsConnection;
  /** Exact admitted context this generation's providers were built from. */
  contextDigest: string;
  retainAuthority?: () => VerifiedAuthority;
  lease?: () => TransportLease;
  /**
   * Held from install until every intake *loop* has finished (not merely until
   * `drain()` resolves), so a buffered accepted delivery cannot arrive after the
   * generation is reaped but before its per-callback lease is taken.
   */
  intakeLease?: TransportLease;
  /**
   * Generic intake subscriptions (RPC + live openings + framework intake).
   * `intakeId` names the framework registration that owns the entry, so a
   * single registration can be retracted without disturbing the others.
   */
  retireFns: Array<{
    intakeId?: string;
    /** Owning generic surface; live owners persist across a drain. */
    owner?: "rpc" | "live";
    drain: () => Promise<void>;
    done: Promise<void>;
    /**
     * Live only: re-create the generic open subscription on the same provider
     * without duplicating it or disturbing accepted sessions/receipts.
     */
    reopen?: () =>
      | { drain: () => Promise<void>; done: Promise<void> }
      | Promise<{ drain: () => Promise<void>; done: Promise<void> }>;
  }>;
  /**
   * Coherent per-generation provider disposal (session-serving control, owned
   * sessions, retained authority). Run only when the generation physically
   * closes, never when its generic intake is drained, so an accepted live
   * session keeps control until it terminates.
   */
  disposeFns: Array<{
    intakeId?: string;
    owner?: "rpc" | "live";
    dispose: () => Promise<void>;
  }>;
  /** Generic intake already drained; no new registrations install here. */
  retired?: boolean;
  /** Resolves when every outstanding intake loop is accounted for. */
  intakeStopped?: Promise<void>;
  /** In-flight coherent disposal, so concurrent calls share one run. */
  disposing?: Promise<void>;
};

export type TrellisSigner = (
  data: Uint8Array,
) => Promise<Uint8Array> | Uint8Array;

export type TrellisAuth = {
  sessionKey: string;
  sign: TrellisSigner;
  currentIat?: () => number;
  /** Current authorization-context digest bound into v1 request and event proofs. */
  contextDigest?: string | (() => string);
  /** Provider-only local verifier for service request and event handling. */
  authorityOwner?: SessionAuthorityOwner;
};

export type TrellisMode = "client" | "service";

export type TrellisOpts<TA extends RuntimeApi> = {
  log?: LoggerLike;
  timeout?: number;
  stream?: string;
  api?: TA;
  state?: RuntimeStateStores;
  stateMigrations?: Readonly<
    Record<string, ResourceMigrations<unknown> | undefined>
  >;
  resourceGeneration?: () => number;
  resourceAvailability?: (name: string) => boolean;
  connection?: TrellisConnection;
  /** Admitted-transport view for boundary checks, set by the connection owner. @internal */
  /**
   * Generation-aware transport provider. When present, finite request and event
   * exchanges acquire a lease for the exact generation they use; when absent,
   * the session keeps its single fixed physical connection. @internal
   */
  transport?: TrellisTransportProvider;
  onSessionNotFound?: () => MaybePromise<void>;
  contractId?: string;
  contractDigest?: string;
};

/** Bounds the caller's request wait, not work already accepted by a service. */
export type RequestOpts = {
  /** Request deadline in milliseconds; expiry returns trellis.request.timeout. */
  timeout?: number;
  /** Cancels the wait with trellis.request.cancelled, including after publication. */
  signal?: AbortSignal;
};

export type EventOpts = {
  mode?: "durable" | "ephemeral";
  /**
   * Contract event consumer group to use for durable service listeners when an
   * event is declared in more than one group.
   */
  group?: string;
  signal?: AbortSignal;
};

export type LiveSubscribeOpts = {
  signal?: AbortSignal;
};

export type LiveInputBuilder<TInput, TEvent> = {
  input(input: TInput): {
    subscribe(
      opts?: LiveSubscribeOpts,
    ): AsyncResult<LiveSubscription<TEvent>, BaseError>;
  };
};

export type LiveHandlerContext<TInput, TEvent> = {
  input: TInput;
  caller: SessionCaller;
  signal: AbortSignal;
  emit(
    event: TEvent,
  ): AsyncResult<
    void,
    SchemaValidationError | ValidationError | UnexpectedError
  >;
};

export type LiveRegistration<TInput, TEvent> = {
  handle(
    handler: (
      context: LiveHandlerContext<TInput, TEvent>,
    ) => unknown | Promise<unknown>,
  ): Promise<void>;
};
