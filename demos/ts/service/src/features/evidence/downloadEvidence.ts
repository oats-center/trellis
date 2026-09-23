import { BaseError, isErr, ok, Result, StoreError } from "@oats-center/trellis";
import type { TransferError } from "@oats-center/trellis";
import type { RpcHandler } from "@oats-center/trellis/service";
import { type participants } from "../../../trellis/index.js";
import type { FieldOpsDeps } from "../../deps.ts";

type Handler = RpcHandler<
  typeof participants.Service.participant,
  "Evidence.Download"
>;

const EVIDENCE_STORE = "uploads";
const TRANSFER_GRANT_TTL_MS = 60_000;
const STORE_VISIBILITY_WAIT_MS = 500;
const STORE_VISIBILITY_POLL_MS = 50;

function stringContext(
  context: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = context[key];
  return typeof value === "string" ? value : undefined;
}

function includesPermissionFailure(error: unknown): boolean {
  if (!(error instanceof Error)) return false;

  const message = error.message.toLowerCase();
  if (
    message.includes("permission") || message.includes("authorization") ||
    message.includes("not permitted")
  ) {
    return true;
  }

  return includesPermissionFailure(error.cause);
}

function transferCauseContext(error: TransferError): Record<string, unknown> {
  const cause = error.cause;
  return cause instanceof BaseError ? cause.getContext() : {};
}

function diagnoseDownloadFailure(args: {
  error: TransferError;
  requestedKey: string;
  storeTtlMs?: number;
  visibilityCheck?: string;
}): string {
  const transferContext = args.error.getContext();
  const causeContext = transferCauseContext(args.error);
  const transferReason = stringContext(transferContext, "reason");
  const causeReason = stringContext(causeContext, "reason");
  const causeKey = stringContext(causeContext, "key");

  if (includesPermissionFailure(args.error)) return "permission_denied";
  if (transferReason === "unknown_store") return "store_binding_missing";
  if (transferReason === "expired") return "transfer_grant_expired";
  if (args.visibilityCheck === "visible_after_wait") return "object_store_race";
  if (causeReason === "timeout") return "object_store_visibility_timeout";
  if (
    causeReason === "not_found" && causeKey && causeKey !== args.requestedKey
  ) {
    return "wrong_key";
  }
  if (causeReason === "not_found" && args.storeTtlMs !== 0) {
    return "missing_or_expired_key";
  }
  if (causeReason === "not_found") return "missing_key";
  return "download_initiation_failed";
}

function annotateDownloadFailure(args: {
  error: TransferError;
  key: string;
  storeTtlMs?: number;
  visibilityCheck?: string;
  visibilityError?: BaseError;
  retryFailed?: boolean;
}): TransferError {
  const transferContext = args.error.getContext();
  const cause = args.error.cause;
  const causeContext = transferCauseContext(args.error);
  const visibilityContext = args.visibilityError?.getContext();

  return args.error.withContext({
    failureStage: "evidence_download_initiate",
    store: EVIDENCE_STORE,
    requestedKey: args.key,
    transferGrantTtlMs: TRANSFER_GRANT_TTL_MS,
    storeTtlMs: args.storeTtlMs,
    transferOperation: args.error.operation,
    transferReason: stringContext(transferContext, "reason"),
    transferCauseName: cause instanceof Error ? cause.name : undefined,
    transferCauseMessage: cause instanceof Error ? cause.message : undefined,
    transferCauseOperation: cause instanceof StoreError
      ? cause.operation
      : undefined,
    transferCauseReason: stringContext(causeContext, "reason"),
    transferCauseKey: stringContext(causeContext, "key"),
    visibilityCheck: args.visibilityCheck,
    visibilityReason: visibilityContext
      ? stringContext(visibilityContext, "reason")
      : undefined,
    retryAfterVisibility: args.retryFailed,
    diagnosis: diagnoseDownloadFailure({
      error: args.error,
      requestedKey: args.key,
      storeTtlMs: args.storeTtlMs,
      visibilityCheck: args.visibilityCheck,
    }),
  });
}

function shouldCheckStoreVisibility(error: TransferError): boolean {
  return stringContext(transferCauseContext(error), "reason") === "not_found";
}

/** Returns a receive transfer grant for stored evidence. */
export function createDownloadEvidenceHandler(
  deps: FieldOpsDeps,
): Handler {
  return async ({ context, input }) => {
    const transferIssuer = deps.transferIssuer;
    const storeTtlMs = transferIssuer.store?.uploads?.binding?.ttlMs;
    const transfer = await transferIssuer.createTransfer({
      direction: "receive",
      store: EVIDENCE_STORE,
      key: input.key,
      sessionKey: context.sessionKey,
      expiresInMs: TRANSFER_GRANT_TTL_MS,
    }).take();

    if (isErr(transfer)) {
      if (shouldCheckStoreVisibility(transfer.error)) {
        const visible = await transferIssuer.store?.uploads?.waitFor?.(
          input.key,
          {
            timeoutMs: STORE_VISIBILITY_WAIT_MS,
            pollIntervalMs: STORE_VISIBILITY_POLL_MS,
          },
        ).take();

        if (visible && !isErr(visible)) {
          const retried = await transferIssuer.createTransfer({
            direction: "receive",
            store: EVIDENCE_STORE,
            key: input.key,
            sessionKey: context.sessionKey,
            expiresInMs: TRANSFER_GRANT_TTL_MS,
          }).take();
          if (!isErr(retried)) {
            return ok({
              ...retried.info,
              size: BigInt(retried.info.size),
              transfer: retried,
            });
          }

          return Result.err(annotateDownloadFailure({
            error: retried.error,
            key: input.key,
            storeTtlMs,
            visibilityCheck: "visible_after_wait",
            retryFailed: true,
          }));
        }

        return Result.err(annotateDownloadFailure({
          error: transfer.error,
          key: input.key,
          storeTtlMs,
          visibilityCheck: visible && isErr(visible)
            ? "not_visible_after_wait"
            : "visibility_check_unavailable",
          visibilityError: visible && isErr(visible)
            ? visible.error
            : undefined,
        }));
      }

      return Result.err(annotateDownloadFailure({
        error: transfer.error,
        key: input.key,
        storeTtlMs,
      }));
    }

    return ok({
      ...transfer.info,
      size: BigInt(transfer.info.size),
      transfer,
    });
  };
}
