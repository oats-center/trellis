/**
 * Trellis service authoring entry point.
 *
 * This subpath exposes the service wrapper and service-side helpers without the
 * internal service runtime implementation.
 *
 * @module
 */

export {
  type HealthCheckResult,
  type HealthCheckStatus,
  runAllServiceHealthChecks,
  runServiceHealthCheck,
  type ServiceHealth,
  type ServiceHealthCheck,
  type ServiceHealthCheckFn,
  type ServiceHealthInfo,
  type ServiceHealthInfoFn,
} from "./runtime/health.ts";
export {
  HealthCheckResultSchema,
  HealthInfoSchema,
} from "./runtime/health_schemas.ts";
export {
  type ConnectedTrellisService,
  type GeneratedServiceParticipant,
  type HealthCheckHandler,
  type HealthInfoHandler,
  type JobHandler,
  type LiveHandler,
  type OperationHandler,
  type OperationRegistration,
  type RpcHandler,
  type RpcName,
  type ServiceEventHandler,
  type ServiceHandlerClient,
  type SqlOutbox,
  type SqlOutboxEventEnqueueFacade,
  type SqlOutboxJobEnqueueFacade,
  type SqlOutboxJobSubmission,
  type SqlOutboxTransactionContext,
  type SqlOutboxTransactionRunner,
  TrellisService,
  type TrellisServiceConnectOpts,
  type TrellisServiceConnectTelemetryOpts,
  type TrellisServiceSqlOutboxCommonOptions,
  type TrellisServiceSqlOutboxExecutorOptions,
  type TrellisServiceSqlOutboxOptions,
} from "./runtime/service.ts";
export {
  createPostgresOutboxSchema,
  createSqliteOutboxSchema,
  createSqlOutboxAdapter,
  defaultSqlOutboxTables,
  dispatchOutbox,
  getSqlOutboxMigrations,
  type InboxRepository,
  type KvOutboxRecord,
  KvOutboxRecordSchema,
  MemoryInboxRepository,
  MemoryOutboxRepository,
  NatsKvInboxRepository,
  NatsKvOutboxRepository,
  OutboxDispatcher,
  type OutboxDispatcherOptions,
  type OutboxDispatchResult,
  type OutboxDispatchRuntime,
  type OutboxJobDispatchOutcome,
  type OutboxKvStore,
  type OutboxMessage,
  type OutboxMessageState,
  outboxMessageToPreparedEvent,
  type OutboxRecordKind,
  type OutboxRepository,
  type PreparedOutboxRecord,
  preparedTrellisEventToOutboxRecord,
  type SqlDialect,
  type SqlExecutor,
  SqlInboxRepository,
  type SqlOutboxAdapter,
  type SqlOutboxMigration,
  type SqlOutboxMigrationOptions,
  SqlOutboxRepository,
  type SqlOutboxTables,
  type SqlRow,
} from "./outbox_inbox.ts";
export {
  ActiveJob,
  decodeJobUpdateEnvelope,
  JobLogEntrySchema,
  JobProgressSchema,
  JobQueue,
  JobRef,
  JobWorkerHostAdapter,
  RetryJobError,
} from "../jobs.ts";
export type {
  Job,
  JobFilter,
  JobHandlerOptions,
  JobIdentity,
  JobLogEntry,
  JobProgress,
  JobsFacade,
  JobsFacadeOf,
  JobSnapshot,
  JobState,
  JobTypeMetadata,
  JobUpdateEnvelope,
  JobUpdatesOptions,
  JobUpdateSubscription,
  JobWorkerHost,
  RetryJobErrorData,
  ServiceInfo,
  TerminalJob,
  WorkerInfo,
} from "../jobs.ts";
export type { RpcHandlerContext } from "../session.ts";
export type {
  ServicePhase,
  ServiceReadinessOpts,
} from "./runtime/lifecycle.ts";
export type {
  ConfigChange,
  ConfigRevision,
  ParticipantConfig,
  ParticipantConfigOf,
} from "./config.ts";
export { type ProviderWebhooks } from "../provider.ts";
export {
  verifyWebhookHmac,
  type WebhookDecideRequest,
  type WebhookDecision,
  type WebhookDelivery,
  type WebhookEndpoint,
  webhookEqual,
  type WebhookField,
  webhookHmac,
  type WebhookHmacAlgorithm,
  type WebhookReceivedArgs,
  type WebhookReceivedOptions,
  type WebhookResponse,
  webhookResponse,
  type WebhookSignatureEncoding,
  withinWebhookWindow,
} from "../webhooks.ts";
