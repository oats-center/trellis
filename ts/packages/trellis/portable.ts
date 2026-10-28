/** Shared portable application API for native and browser entrypoints. */
export {
  classifyBrowserAuthError,
  createAuth,
  isRecoverableBrowserAuthError,
} from "./auth.ts";
export type {
  BrowserAuthRecoveryClassification,
  BrowserAuthRecoveryKind,
  NatsConnectOptions,
} from "./auth.ts";
export { isJsonValue } from "./participant.ts";
export type { InferSchemaType, JsonValue } from "./participant.ts";
export { TrellisConnection } from "./connection.ts";
export type { TrellisConnectionStatus } from "./connection.ts";
export {
  buildCursorPage,
  buildPageResponse,
  CursorPageInfoSchema,
  CursorPageSchema,
  CursorQuerySchema,
  normalizeCursorQuery,
  normalizePageQuery,
  PageRequestSchema,
  PageResponseSchema,
} from "./participant.ts";
export type {
  CursorPage,
  CursorPageInfo,
  CursorPageResponseSchema,
  CursorQuery,
  CursorQueryOptions,
  InferRuntimeRpcError,
  NormalizedCursorQuery,
  PageRequest,
  PageResponse,
  RpcErrorClass,
  RuntimeRpcErrorDesc,
  SerializableErrorData,
  TrellisValidationExtension,
  TrellisValidationIssueHint,
} from "./participant.ts";
export {
  AsyncResult,
  BaseError,
  err,
  isErr,
  isOk,
  ok,
  Result,
} from "@oatscenter/result";
export type { MaybeAsync } from "@oatscenter/result";
export type { ClientOpts } from "./client.ts";
export type { CallerParticipant, CallerRuntime } from "./caller.ts";
export { PaginationError } from "./pagination.ts";
export {
  decodePaginationCursor,
  encodePaginationCursor,
  paginationQueryDigest,
} from "./auth/protocol_wasm.ts";
export type { PaginationErrorData } from "./pagination.ts";
export type { CallerUser } from "./auth/protocol_wasm.ts";
export type {
  ClientAuthContinuation,
  ClientAuthOptions,
  ClientAuthRequiredContext,
  ClientLogoutOptions,
  ClientResourceMigrations,
  ConnectedTrellisClient,
  TrellisClientConnectArgs,
} from "./client_connect.ts";
export { TrellisClient } from "./client_connect.ts";
export type {
  OwnAuthorityView,
  OwnAuthoritySnapshot,
  OwnAuthorizedAction,
} from "./own_authority.ts";
export type { AppRequestDescriptor } from "./generated_support.ts";
export {
  newUlid,
  parseTimestamp,
  parseUlid,
  timestamp,
  type Timestamp,
  type Ulid,
} from "./values.ts";
export type { KvWatchItem, ResourceRevision, TypedKvEntry } from "./kv.ts";
export type { TrellisErrorInstance } from "./errors/index.ts";
export {
  AuthError,
  KVError,
  OperationAlreadyTerminalError,
  OperationMismatchError,
  OperationNotFoundError,
  RemoteError,
  StoreError,
  TransferError,
  TransportError,
  TrellisError,
  UnexpectedError,
  ValidationError,
} from "./errors/index.ts";
export { TypedStoreEntry } from "./store_entry.ts";
export type {
  StoreBody,
  StoreInfo,
  StoreListOptions,
  StoreListPage,
  StoreOpenOptions,
  StorePutOptions,
  StoreStatus,
  StoreWaitOptions,
} from "./store.ts";
export { decodeTransferGrant, FileInfoSchema } from "./transfer.ts";
export type {
  FileInfo,
  ReceiveTransferGrant,
  ReceiveTransferHandle,
  SendTransferGrant,
  SendTransferHandle,
  TransferBody,
  TransferGrant,
} from "./transfer.ts";
export type {
  AcceptedOperationEvent,
  CancelledOperationEvent,
  CompletedOperationEvent,
  CompletedTransfer,
  FailedOperationEvent,
  OperationControlError,
  OperationEvent,
  OperationInputBuilder,
  OperationLifecycleError,
  OperationObserverCallbacks,
  OperationRef,
  OperationRefData,
  OperationSignalAck,
  OperationSnapshot,
  OperationStartOptions,
  OperationState,
  OperationTransferProgress,
  OperationTransferStartOptions,
  OperationWatchOptions,
  ProgressOperationEvent,
  ProgressOperationSnapshot,
  StartedOperationEvent,
  StartedTransfer,
  TerminalOperation,
  TransferCapableOperationInputBuilder,
  TransferOperationBuilder,
  TransferOperationEvent,
  TransferOperationSnapshot,
  UpdateOperationEvent,
} from "./operations.ts";
export { controlSubject, OperationInvoker } from "./operations.ts";
export { LiveSubscription } from "./live/subscription.ts";
export { LiveEnd, LiveStreamError } from "./live/types.ts";
export type {
  AcceptedOperation,
  EventListenerContext,
  EventName,
  EventOpts,
  EventPayload,
  EventType,
  HandlerJobQueue,
  HandlerJobsFacade,
  HandlerKvFacade,
  HandlerStoreHandle,
  HandlerTrellis,
  HandlerTrellisForContract,
  InternalCaller,
  LiveInputBuilder,
  LiveSubscribeOpts,
  OperationHandlerContext,
  OperationHandlerErrorOf,
  OperationRegistration,
  OperationRuntimeHandle,
  OperationTransferContextOf,
  OperationTransferHandle,
  OperationUpdateOf,
  PreparedTrellisEvent,
  RequestOpts,
  RpcHandlerContext,
  RpcInputOf,
  RpcMethodNameOf,
  RpcOutputOf,
  RpcRequestErrorOf,
  RuntimeStateStoresForContract,
  RuntimeStateStoreShape,
  SessionCaller,
  StateFacade,
  StateWatch,
  TrellisEventHeader,
  TrellisEventMessage,
  ValueStateStoreClient,
  VerifiedCaller,
} from "./session.ts";
export { ResourceUnavailableError, StateConflictError } from "./session.ts";
export type { TrellisAuth } from "./session.ts";
