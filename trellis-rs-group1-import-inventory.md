# Group 1 Import Inventory

This is a finite **Trellis export-boundary** inventory of the current working
tree, not a proposed export policy. HEAD is
`0411d4eee7931b514775bc20f08660e675609cba`; the uncommitted recovery changes are
included. No exports were changed to produce this inventory.

Scope:

- A: handwritten `demos/`, `web/src/`, and `docs/examples/orders/`, including
  their tests and Svelte imports.
- B: every Trellis runtime import the current native TS renderer can emit,
  including participant metadata branches, not just the Orders/app subset.
- C: `ts/packages/trellis-test/` and `ts/tools/`. Generated test-kit modules
  belong to B; handwritten test-kit imports belong to C.
- D: `ts/packages/trellis/service/`, `device.ts`, and `device/`, excluding their
  tests. Internal relative Trellis imports are listed separately from public
  package requirements.
- Names are deduplicated within an entrypoint/module row. Import aliases do not
  count as additional exported symbols. Local feature-module imports and
  third-party/standard-library symbols are not Trellis export requirements and
  are outside this inventory. Generated API-specific types and actions are
  accessed through their generated package, not the runtime root.
- Obsolete `.trellis/` leftovers, `node_modules/`, and duplicate npm build
  output are excluded. This is not an inventory of all integration-test or
  documentation prose examples across the repository.

`V` means imported as a runtime value somewhere in the group; `T` means
type-only in that group. A value such as `Result` may also be used as a type.

## A. Handwritten Demos and Apps

In the following table, `/suffix` means `@qlever-llc/trellis/suffix`.

| Entrypoint                   | Runtime values                                                                                                                                         | Type-only names                                                                                                                                                                                                                                                                                                                                                                                         |
| ---------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `@qlever-llc/trellis`        | `AsyncResult`, `BaseError`, `isErr`, `isJsonValue`, `ok`, `Result`, `StoreError`, `TransferError`, `TrellisClient`, `TrellisDevice`, `UnexpectedError` | `JsonValue`, `ReceiveTransferGrant`                                                                                                                                                                                                                                                                                                                                                                     |
| `/errors`                    | `TransportError`                                                                                                                                       | None                                                                                                                                                                                                                                                                                                                                                                                                    |
| `/service`                   | None                                                                                                                                                   | `ConnectedTrellisService`, `JobHandler`, `OperationHandler`, `RpcHandler`                                                                                                                                                                                                                                                                                                                               |
| `/service/deno`              | `TrellisService`                                                                                                                                       | None                                                                                                                                                                                                                                                                                                                                                                                                    |
| `/device/deno`               | `checkDeviceActivation`                                                                                                                                | None                                                                                                                                                                                                                                                                                                                                                                                                    |
| `/auth/file`                 | `FileAuthorizationContextStore`                                                                                                                        | None                                                                                                                                                                                                                                                                                                                                                                                                    |
| `/auth/browser`              | `classifyBrowserAuthError`, `decodeTrellisHttpError`, `getOrCreatePortalBinding`, `portalRedirectLocation`                                             | None                                                                                                                                                                                                                                                                                                                                                                                                    |
| `/auth`                      | None                                                                                                                                                   | `AuthDeploymentAuthorityGetResponse`, `DeploymentAuthority`, `DeploymentAuthorityCapabilityNeed`, `DeploymentAuthorityContractNeed`, `DeploymentAuthorityKind`, `DeploymentAuthorityMaterialization`, `DeploymentAuthorityNeeds`, `DeploymentAuthorityPlan`, `DeploymentAuthorityPlanBreakingChange`, `DeploymentAuthorityResourceNeed`, `DeploymentAuthoritySurface`, `DeploymentAuthoritySurfaceNeed` |
| `@qlever-llc/result`         | `AsyncResult`, `BaseError`, `isErr`, `UnexpectedError`                                                                                                 | `Result`                                                                                                                                                                                                                                                                                                                                                                                                |
| `@qlever-llc/trellis-svelte` | `createDeviceActivationController`, `createPortalFlow`, `createTrellisApp`, `TrellisProvider`                                                          | `DeviceActivationAuth`, `DeviceActivationOperationRef`, `TrellisClientFor`                                                                                                                                                                                                                                                                                                                              |
| `@qlever-llc/trellis-test`   | `TrellisTestRuntime`                                                                                                                                   | None                                                                                                                                                                                                                                                                                                                                                                                                    |
| Local generated packages     | `apis`, `participants`                                                                                                                                 | The same namespaces also occur in type positions                                                                                                                                                                                                                                                                                                                                                        |

The runtime/result/Svelte imports use existing entrypoints. `TrellisDevice` is a
non-browser consumer requirement, not a request to add it to `browser.ts`.
Orders' `TrellisTestRuntime` check uses the copied source test kit; it does not
prove installation of a published npm test-kit package.

Representative evidence:

- `demos/ts/device/src/main.ts:1-3`
- `demos/ts/service/src/main.ts:1-2`, `src/deps.ts:1-11`, and feature
  handlers/tests
- `demos/app/src/lib/trellis.ts:2-6` and its app layout
- `web/src/lib/authority_console.ts:2-14`, `device_activation.ts:1-7`,
  `portal_login.ts:1`, and console/login Svelte imports
- `docs/examples/orders/main.ts:1-2`, `service.ts:1-2`, `service_test.ts:2-3`

## B. Generated TypeScript

The renderer has a closed set of **14 Trellis root imports: 13 values and one
type**. It does not import `/participant`, `/jobs`, or `/contracts`.

| Imported symbol                        | Kind | Ordinary root | Browser value export  | Producer/use                            |
| -------------------------------------- | ---- | ------------- | --------------------- | --------------------------------------- |
| `eventActions`                         | V    | Present       | Present, approved     | Event descriptors                       |
| `feedAction`                           | V    | Present       | Present, approved     | Feed descriptors                        |
| `operationAction`                      | V    | Present       | Present, approved     | Operation descriptors                   |
| `rpcAction`                            | V    | Present       | Present, approved     | RPC descriptors                         |
| `schema`                               | V    | Present       | Present, pre-existing | Descriptor schemas                      |
| `TrellisError`                         | V    | Present       | Present, pre-existing | Generated error classes                 |
| `SerializableErrorData`                | T    | Present       | Not a JS value        | Generated error types                   |
| `PARTICIPANT_RUNTIME`                  | V    | Present       | Present, approved     | Participant runtime metadata            |
| `runtimeApiFromActions`                | V    | Present       | Present, approved     | Participant action-derived runtime APIs |
| `PARTICIPANT_STATE_METADATA`           | V    | Present       | Present, approved     | Participant state metadata              |
| `PARTICIPANT_EVENT_CONSUMERS_METADATA` | V    | Present       | Absent                | Participant event-consumer metadata     |
| `PARTICIPANT_JOBS_METADATA`            | V    | Present       | Absent                | Participant job metadata                |
| `PARTICIPANT_KV_METADATA`              | V    | Present       | Absent                | Participant KV metadata                 |
| `PARTICIPANT_STORE_METADATA`           | V    | Present       | Absent                | Participant store metadata              |

Evidence: `rust/crates/codegen-ts/src/lib.rs:227-237`, `:673-682`, and
`:940-945`; metadata fields are selected at `:336-350`. Native emission removes
unused imports, so an individual emitted package need not import all 14.

Generated modules additionally import their own `API`/`API_DIGEST`, schema
constants, `Types` namespace, and referenced API namespaces. Those are ordinary
intra-package imports, not additional Trellis runtime exports.

The last four absent browser values are **not automatically four required
browser repairs**. Their presence in the generator's full vocabulary does not
establish a browser-supported participant requirement. The actual app build
proved the seven approved helpers were necessary and sufficient for that app. No
broader browser approval is recorded here.

The npm root's `types` condition selects `index.d.ts`, while its browser runtime
condition selects `browser.js`. Consequently a successful type check alone does
not prove that a root-imported runtime value exists in the browser entrypoint.

## C. Test Kit and Tooling

| Entrypoint                 | Runtime values                                | Type-only names                                                                                                                | npm availability        |
| -------------------------- | --------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------ | ----------------------- |
| `@qlever-llc/trellis`      | `createAuth`, `TrellisClient`                 | `CallerParticipant`, `CallerRuntime`, `ClientAuthContinuation`, `ClientAuthOptions`, `ClientAuthRequiredContext`, `ClientOpts` | Present                 |
| `/auth/browser`            | `createPortalBinding`, `fetchPortalFlowState` | `PortalBinding`                                                                                                                | Present                 |
| `/telemetry`               | `recordTrellisDuration`                       | None                                                                                                                           | Present                 |
| `/participant`             | `participantPresentation`                     | `GeneratedParticipantEvidence`, `ParticipantPresentation`                                                                      | Subpath absent          |
| Generated test-kit package | `apis`, `participants`                        | Generated auth/state types below                                                                                               | Local generated package |

The test kit also uses inline generated-package type imports:
`apis.auth.AuthDevicesProvisionInput`, `apis.auth.AuthDevicesProvisionOutput`,
`apis.state.StateAdminGetInput`, `apis.state.StateAdminGetOutput`,
`apis.state.StateAdminListInput`, `apis.state.StateAdminListOutput`,
`apis.state.StateAdminDeleteInput`, `apis.state.StateAdminDeleteOutput`.

Evidence:

- `ts/packages/trellis-test/src/types.ts:1-8`
- `src/runtime.ts:1-8`, `src/admin_client.ts:1-5`
- `src/admin/auth_flow.ts:1-6`, `src/admin/metrics.ts:1`
- `src/admin/deployment.ts:1-5`
- `src/admin/methods.ts:1-2`
- Inline type imports in `src/runtime.ts` and `src/admin_client.ts`

`ts/tools/` contains no direct named imports from the Trellis package family.
Its compiler/filesystem/build-helper imports do not establish new runtime API
requirements.

The three `/participant` names are a **single unresolved package-boundary
decision**, not three separate requests:

- `GeneratedParticipantEvidence` already has the identical public type alias
  `CallerParticipant` (`ts/packages/trellis/caller.ts`).
- `ParticipantPresentation` can be named locally using
  `ReturnType<typeof participantPresentation>` if the function is available.
- `participantPresentation` is the runtime operation that validates and returns
  canonical participant evidence. It is not currently exported from the root.

Thus the previously proposed minimum remains one named root value export, not a
new `/participant` subpath or duplicated validation. This inventory does not
approve or implement that proposal.

## D. Service and Device Implementation

These implementations do not import the public `@qlever-llc/trellis` package
root or its subpaths. They import sibling source modules relatively. Such
imports require files in the package, **not public package exports**.

### Participant Boundary

Paths below are relative to `ts/packages/trellis/`.

| Source module                          | Values                                                                                                                       | Types                                                                          |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------ |
| `participant.ts`                       | `ContractResourceBindingsSchema`                                                                                             | `EventDesc`, `InferSchemaType`, `JsonValue`                                    |
| `participant_runtime/api.ts`           | None                                                                                                                         | `PermissionAtom`, `RuntimeApi`                                                 |
| `participant_runtime/artifacts.ts`     | None                                                                                                                         | `GeneratedParticipantEvidence`                                                 |
| `participant_runtime/descriptors.ts`   | None                                                                                                                         | `ActionDescriptor`                                                             |
| `participant_runtime/json.ts`          | `isJsonValue`                                                                                                                | None                                                                           |
| `participant_runtime/metadata.ts`      | `PARTICIPANT_EVENT_CONSUMERS_METADATA`, `PARTICIPANT_JOBS_METADATA`, `PARTICIPANT_KV_METADATA`, `PARTICIPANT_STATE_METADATA` | `ParticipantJobsMetadata`, `ParticipantKvMetadata`, `ParticipantStateMetadata` |
| `participant_runtime/participant.ts`   | `getParticipantRuntime`                                                                                                      | `GeneratedParticipant`                                                         |
| `participant_runtime/resolution.ts`    | `resolveParticipantPresentation`                                                                                             | None                                                                           |
| `participant_runtime/schemas.ts`       | None                                                                                                                         | `ContractEventConsumers`                                                       |
| `participant_runtime/surface_names.ts` | `lowerCamelSurfaceName`                                                                                                      | `ConnectedActionName`                                                          |

Evidence: `service/runtime/service.ts:30-53`, `bootstrap.ts:19-21`,
`internal_connect.ts:4-5`, `core.ts:3-6`, `subscription.ts:2-3`,
`transfer.ts:17`, `transfer/download.ts:4`, `transfer/upload.ts:2`, and
`device.ts:10-16,36,63-66`.

Notably, service/device code imports **`resolveParticipantPresentation`**, not
the test kit's `participantPresentation`. The former is an internal resolution
operation. Its use is not a reason to expose it publicly.

### Other Shared Trellis Imports

The following completes the Trellis-owned imports outside the participant
boundary above. Names are the original imported names; local aliases such as
`SessionAuth`, `noopLogger`, `PublicActiveJob`, and `ResultType` are not
additional symbols. Rows grouping a module family are explicitly labeled as
such.

| Source module/family                           | Imported symbols                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| ---------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `@qlever-llc/result`                           | `AsyncResult`, `BaseError`, `err`, `isErr`, `MaybeAsync`, `ok`, `Result`, `UnexpectedError`                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| `auth.ts` and `auth/*`                         | `AuthorizationContextBundle`, `AuthorizationContextBundleSchema`, `AuthorizationContextCache`, `AuthorizationContextPersistence`, `AuthorizationProviderCache`, `base64urlDecode`, `base64urlEncode`, `createAuth`, `decodeTrellisHttpError`, `deriveDeviceConfirmationCode`, `deriveDeviceIdentity`, `estimateMidpointClockOffsetMs`, `MemoryAuthorizationContextStore`, `SESSION_PROOF_FORMAT_V1`, `sessionProofRequestDigest`, `sha256`, `startAuthorizationContextRefresh`, `TrellisAuth`, `utf8`, `verifyDeviceConfirmationCode`, `waitForDeviceActivation` |
| `caller.ts`                                    | `CallerRuntime`, `createCallerRuntime`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `codec.ts`                                     | `JsonValue`, `parseSchema`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                       |
| `connection.ts`                                | `observeNatsTrellisConnection`, `TrellisConnection`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `device.ts` (from filesystem adapter)          | `resumeDeviceActivationWithDeps`, `startDeviceActivationWithDeps`, `TrellisDeviceActivatedActivationState`, `TrellisDeviceActivationArgs`, `TrellisDeviceLocalActivationState`, `TrellisDevicePendingActivationState`                                                                                                                                                                                                                                                                                                                                            |
| `env.ts`                                       | `getEnv`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `errors/index.ts` and individual error modules | `AuthError`, `OperationAlreadyTerminalError`, `OperationMismatchError`, `OperationNotFoundError`, `StoreError`, `TransferError`, `TransportError`, `TrellisErrorInstance`, `UnexpectedError`, `ValidationError`                                                                                                                                                                                                                                                                                                                                                  |
| `globals.ts`                                   | `logger`, `LoggerLike`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `health_transport.ts`                          | `publishHealthHeartbeatSample`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                   |
| `internal_sdk/generated/health/mod.ts`         | `HealthHeartbeatSample`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                          |
| `jobs.ts`                                      | `ActiveJob`, `decodeJobUpdateEnvelope`, `getActiveJobSnapshot`, `JobHandlerOptions`, `JobLogEntry`, `JobNotEnqueuedError`, `JobProgress`, `JobRef`, `JobSnapshot`, `JobSubmitOutcome`, `JobUpdatesOptions`, `JobUpdateSubscription`, `JobWorkerHostAdapter`, `RetryJobError`, `runWithActiveJobContext`, `TerminalJob`                                                                                                                                                                                                                                           |
| `kv.ts`                                        | `TypedKV`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| `provider.ts`                                  | `createProviderRuntime`, `PROVIDER_CALLER`, `ProviderCaller`, `ProviderHandlerClient`, `ProviderRuntime`                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `runtime_transport.ts`                         | `DEFAULT_RUNTIME_MAX_RECONNECT_ATTEMPTS`, `DEFAULT_SERVICE_RUNTIME_WAIT_ON_FIRST_CONNECT`, `loadDefaultRuntimeTransport`, `selectRuntimeTransportServers`                                                                                                                                                                                                                                                                                                                                                                                                        |
| `store.ts`                                     | `StoreInfo`, `StoreWaitOptions`, `TypedStore`, `TypedStoreEntry`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                 |
| `telemetry/carrier.ts`                         | `createMapCarrier`, `injectTraceContext`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                         |
| `telemetry/init.ts`                            | `initTelemetry`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `telemetry/mod.ts`                             | `recordTrellisDuration`, `recordTrellisError`, `TrellisErrorMetricAttributes`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `transfer.ts`                                  | `FileInfo`, `MAX_TRANSFER_CHUNK_BYTES`, `ReceiveTransferGrant`, `SendTransferGrant`                                                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| `transfer_protocol.ts`                         | `transferFrameProofPayload`                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                      |

`session.ts` has the largest shared import set; split here for readability:

| Kind                    | Imported symbols                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| ----------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Values                  | `annotateHandlerBoundaryError`, `buildRuntimeOperationSnapshot`, `createTrellisInternal`, `isOperationDeferred`, `isResultLike`, `isTerminalRuntimeOperationSnapshot`, `safeJson`, `Trellis`, `verifyLocalAuthorization`                                                                                                                                                                                                                                                                                                                                                          |
| Types, handlers/facades | `AcceptedOperation`, `ActiveEventFacade`, `ActiveEventPublishFacade`, `EventListenerContext`, `EventOpts`, `FeedEventOf`, `FeedHandlerContext`, `FeedInputOf`, `FeedRegistration`, `FeedsOf`, `HandlerFn`, `HandlerTrellis`, `MethodsOf`, `OperationHandlerContext`, `OperationHandlerErrorOf`, `OperationInputOf`, `OperationOutputOf`, `OperationProgressOf`, `OperationRegistration`, `OperationRuntimeHandle`, `OperationsOf`, `OperationTransferContextOf`, `OperationTransferHandle`, `OperationUpdateOf`, `PreparedTrellisEvent`, `RpcHandlerContext`, `RpcHandlerErrorOf` |
| Types, internal runtime | `RuntimeOperationAcceptedEnvelope`, `RuntimeOperationController`, `RuntimeOperationControlRequest`, `RuntimeOperationDesc`, `RuntimeOperationRecord`, `RuntimeOperationSignal`, `RuntimeOperationSnapshot`, `RuntimeOperationState`, `RuntimeOperationTransferProgress`, `RuntimeStateStoresForContract`, `TrellisAuth`, `TrellisDurableEventConsumerBeforeReadinessCheckHook`, `TrellisMode`, `TrellisOpts`, `VerifiedCaller`                                                                                                                                                    |

### Internal Service Wiring

These imports stay within `service/`; listing them does not propose any public
export. Paths are relative to that directory.

| Source module                                    | Imported symbols                                                                                                                                                                                                                                                                   |
| ------------------------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `outbox_inbox.ts`                                | `defaultSqlOutboxTables`, `OutboxDispatcher`, `OutboxDispatcherOptions`, `OutboxDispatchRuntime`, `OutboxJobDispatchOutcome`, `OutboxMessage`, `PreparedOutboxRecord`, `preparedTrellisEventToOutboxRecord`, `SqlDialect`, `SqlExecutor`, `SqlOutboxRepository`, `SqlOutboxTables` |
| `runtime/bootstrap.ts`                           | `closeFailedServiceBootstrapConnection`, `fetchServiceBootstrapInfo`, `loadDefaultServiceRuntimeDeps`                                                                                                                                                                              |
| `runtime/core.ts`                                | `TrellisServiceRuntime`, `TrellisServiceRuntimeFor`                                                                                                                                                                                                                                |
| `runtime/health.ts`                              | `ServiceHealth`, `ServiceHealthCheckFn`, `ServiceHealthInfoFn`, `ServiceHealthRuntime`                                                                                                                                                                                             |
| `runtime/logger.ts`                              | `serviceRuntimeLogger`                                                                                                                                                                                                                                                             |
| `runtime/runtime.ts`                             | `NatsConnectOpts`, `TrellisServiceRuntimeDeps`                                                                                                                                                                                                                                     |
| `runtime/service.ts`                             | `createConnectedService`, `GeneratedServiceParticipant`, `ResourceBindings`, `Trellis`, `TrellisServiceConnectOpts`, `TrellisServiceInternalConnectArgs`, `TrellisServiceSession`                                                                                                  |
| `runtime/transfer.ts`                            | `ServiceTransfer`, `StoredTransfer`                                                                                                                                                                                                                                                |
| `runtime/internal_jobs/active-job.ts`            | `ActiveJob`, `ActiveJobRuntimeError`, `JobCancellationToken`                                                                                                                                                                                                                       |
| `runtime/internal_jobs/bindings.ts`              | `JobsBinding`, `JobsQueueBinding`, `JobsRuntimeBinding`                                                                                                                                                                                                                            |
| `runtime/internal_jobs/cancellation-registry.ts` | `ActiveJobCancellationRegistry`                                                                                                                                                                                                                                                    |
| `runtime/internal_jobs/heartbeat.ts`             | `startWorkerHeartbeatLoop`                                                                                                                                                                                                                                                         |
| `runtime/internal_jobs/job-manager.ts`           | `ActiveJob`, `JobCancellationToken`, `JobManager`, `JobProcessError`, `JobProcessOutcome`, `prepareJobSubmission`                                                                                                                                                                  |
| `runtime/internal_jobs/key-coordinator.ts`       | `ActiveSlotLease`, `createNatsJobKeyCoordinator`, `JobAdmissionOutcome`, `JobKeyActiveSlot`, `JobKeyConcurrencyBinding`, `JobKeyCoordinator`, `JobQueuePolicyBinding`, `normalizeJobKeyPolicy`, `NormalizedJobKeyPolicy`, `ReplacedQueuedJob`                                      |
| `runtime/internal_jobs/projection.ts`            | `isTerminal`, `jobFromWorkEvent`                                                                                                                                                                                                                                                   |
| `runtime/internal_jobs/runtime-worker.ts`        | `startNatsWorkerHostFromBinding`                                                                                                                                                                                                                                                   |
| `runtime/internal_jobs/types.ts`                 | `Job`, `JobContext`, `JobEvent`, `JobEventSchema`, `JobLineage`, `JobLogEntry`, `JobProgress`, `JobState`, `JobTrigger`, `JobWaitEdge`, `JobWaitTarget`, `PreparedJobSubmission`, `PreparedJobSubmissionSchema`, `WorkerHeartbeat`                                                 |
| `runtime/transfer/download.ts`                   | `DownloadSession`                                                                                                                                                                                                                                                                  |
| `runtime/transfer/protocol.ts`                   | `DEFAULT_TRANSFER_CHUNK_BYTES`, `DOWNLOAD_SUBJECT_PREFIX`, `fileInfoFromStoreInfo`, `parseSeq`, `publishError`, `replyError`, `TRANSFER_CONTROL_HEADER`, `TRANSFER_EOF_HEADER`, `TRANSFER_SEQUENCE_HEADER`, `UPLOAD_SUBJECT_PREFIX`                                                |
| `runtime/transfer/queue.ts`                      | `AsyncChunkQueue`, `AsyncValueBroadcaster`, `deferred`                                                                                                                                                                                                                             |
| `runtime/transfer/upload.ts`                     | `effectiveUploadMaxBytes`, `raceCancellation`, `UploadSession`                                                                                                                                                                                                                     |

`service/deno.ts` and `service/node.ts` are export facades, not separate sets of
runtime imports. The device filesystem adapter now uses explicit `node:`
imports; no Deno ambient namespace or ORM adapter is required.

## Decision Summary

1. **A:** existing runtime entrypoints cover the handwritten imports. Consumer
   verification still has the C test-kit blocker when running Orders.
2. **B:** 14 root symbols comprise the generator vocabulary. All exist in the
   ordinary root. The seven demo-proven browser additions are approved and
   implemented; four other metadata values are intentionally not inferred to be
   browser requirements from this inventory alone.
3. **C:** `/participant` accounts for the known unresolved package boundary: one
   function and two types. One existing function exported from the ordinary root
   can resolve it without a new subpath; approval remains pending.
4. **D:** relative implementation imports do not imply public root, browser,
   `/participant`, or `/jobs` exports. In particular, private service resource
   bindings, worker machinery, and participant resolution stay internal.

This inventory is not an acceptance report, a new test layer, or authorization
to export every listed internal name. No export changes were made in this pass.
