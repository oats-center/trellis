import {
  type AuthenticatedCaller,
  type CatalogActionIdentity,
  type OriginalEventPublication,
  type SessionRequest,
} from "../auth/protocol_wasm.ts";
import { type Msg } from "@nats-io/nats-core";
import { type SessionAuthorityOwner } from "../auth/authority.ts";
import { type PermissionAtom as DescriptorPermissionAtom } from "../participant_runtime/api.ts";
import { AuthError } from "../errors/index.ts";
import { Result, err, ok } from "@oatscenter/result";

import { recordCatalogDuration } from "../telemetry/mod.ts";

export type RuntimeRpcErrorDesc = {
  type: string;
  schema?: unknown;
  fromSerializable(data: unknown): Error;
};

export type InferRuntimeRpcError<T> = T extends {
  fromSerializable(data: unknown): infer TError;
}
  ? TError
  : never;

/** Caller projection returned after a context-bound proof is verified locally. */
export type VerifiedCaller = AuthenticatedCaller & { type: "verified" };

/** Internal system caller used only by explicitly unauthenticated surfaces. */
export type InternalCaller = {
  type: "internal";
  capabilities: readonly [];
};

/** Caller data exposed to Trellis handlers. */
export type SessionCaller = VerifiedCaller | InternalCaller;

export type LocalAuthorizationRequestMessage = Pick<
  Msg,
  "data" | "headers" | "reply" | "subject"
>;

export type LocalAuthorizationEventMessage = Pick<
  Msg,
  "data" | "headers" | "subject"
>;

export type LocalAuthorizationArgs =
  | {
      kind: "request" | "provider-request";
      cache: SessionAuthorityOwner | undefined;
      message: LocalAuthorizationRequestMessage;
      proofPayload?: Uint8Array;
      permission: DescriptorPermissionAtom | undefined;
      requiredCapabilities: readonly string[];
    }
  | {
      kind: "transfer";
      cache: SessionAuthorityOwner | undefined;
      message: LocalAuthorizationRequestMessage;
      proofPayload?: Uint8Array;
      permission: DescriptorPermissionAtom | undefined;
      requiredCapabilities: readonly string[];
      transferId: string;
      providerConnectionId: string;
      consumerConnectionId: string;
    }
  | {
      kind: "event";
      cache: SessionAuthorityOwner | undefined;
      message: LocalAuthorizationEventMessage;
      originalPublication?: OriginalEventPublication;
      permission: DescriptorPermissionAtom | undefined;
      descriptorIdentity: string;
      requiredCapabilities: readonly string[];
    };

export class EventVerificationAuthError extends AuthError {
  constructor(readonly retryable: boolean) {
    super({
      reason: retryable ? "authorization_unavailable" : "invalid_signature",
    });
  }
}

/** Verifies one received request or event through the provider-local WASM cache. */
export async function verifyLocalAuthorization(
  args: LocalAuthorizationArgs,
): Promise<Result<VerifiedCaller, AuthError>> {
  const startedAt = performance.now();
  let outcome = "invalid";
  try {
    if (!args.permission) {
      outcome = "denied";
      return err(new AuthError({ reason: "insufficient_permissions" }));
    }
    if (!args.cache) {
      outcome = "unavailable";
      return err(new AuthError({ reason: "invalid_signature" }));
    }

    for (const name of [
      "proof",
      "authorization-context",
      "session-key",
      "iat",
      "request-id",
      "Trellis-Api-Generation",
      "Trellis-Accepted-Revision",
      "Trellis-Session-Event",
      "Nats-Msg-Id",
      "Trellis-Event-Time",
      "Trellis-Event-Descriptor",
    ]) {
      if ((args.message.headers?.values(name).length ?? 0) > 1)
        return err(new AuthError({ reason: "invalid_signature" }));
    }
    const proof = args.message.headers?.get("proof");
    const contextDigest = args.message.headers?.get("authorization-context");
    if (!proof || !contextDigest || !args.permission) {
      return err(new AuthError({ reason: "missing_proof" }));
    }

    try {
      const apiId = `${args.permission.apiId}@${args.permission.apiVersion}`;
      const action = toCatalogAction(args.permission);
      const eventPublication: unknown =
        args.kind === "event"
          ? JSON.parse(
              args.message.headers?.get("Trellis-Session-Event") || "null",
            )
          : null;
      const apiGeneration =
        args.kind === "event"
          ? eventPublication &&
            typeof eventPublication === "object" &&
            "apiGeneration" in eventPublication
            ? eventPublication.apiGeneration
            : undefined
          : args.message.headers?.get("Trellis-Api-Generation");
      const acceptedRevision =
        args.kind === "event"
          ? eventPublication &&
            typeof eventPublication === "object" &&
            "acceptedRevision" in eventPublication
            ? eventPublication.acceptedRevision
            : undefined
          : args.message.headers?.get("Trellis-Accepted-Revision");
      if (
        typeof apiGeneration !== "string" ||
        typeof acceptedRevision !== "string"
      )
        return err(new AuthError({ reason: "missing_proof" }));
      if (args.kind !== "event") {
        const iatHeader = args.message.headers?.get("iat");
        const requestId = args.message.headers?.get("request-id");
        const reply = args.message.reply;
        const iat = Number(iatHeader);
        if (!Number.isSafeInteger(iat) || !requestId || !reply) {
          return err(new AuthError({ reason: "invalid_signature" }));
        }
        const request: SessionRequest = {
          authorityDigest: contextDigest,
          apiId,
          apiGeneration,
          acceptedRevision,
          action: action.name,
          subject: args.message.subject,
          replySubject: reply,
          issuedAt: iat,
          requestId,
        };
        const verified = await args.cache.verifyRequest(
          request,
          args.proofPayload ?? args.message.data,
          proof,
          action,
          args.kind === "provider-request" ? "provider" : "consumer",
        );
        try {
          const sessionKey = args.message.headers?.get("session-key");
          if (
            sessionKey &&
            sessionKey !== verified.caller.authority.sessionPublicKey
          )
            return err(new AuthError({ reason: "invalid_signature" }));
          outcome = "ok";
          return ok({ type: "verified", ...verified.caller.caller });
        } finally {
          verified.caller.release();
        }
      } else {
        const eventId = args.message.headers?.get("Nats-Msg-Id");
        const eventTime = args.message.headers?.get("Trellis-Event-Time");
        const descriptorIdentity = args.message.headers?.get(
          "Trellis-Event-Descriptor",
        );
        if (
          !eventId ||
          !eventTime ||
          descriptorIdentity !== args.descriptorIdentity
        ) {
          return err(new AuthError({ reason: "invalid_signature" }));
        }
        const caller = await args.cache.verifyEvent(
          {
            authorityDigest: contextDigest,
            apiId,
            apiGeneration,
            acceptedRevision,
            action: action.name,
            subject: args.message.subject,
            eventId,
            eventTime,
          },
          args.message.data,
          proof,
          action,
          args.originalPublication,
        );
        const sessionKey = args.message.headers?.get("session-key");
        if (sessionKey && sessionKey !== caller.sessionPublicKey)
          return err(new AuthError({ reason: "invalid_signature" }));
        outcome = "ok";
        return ok({ type: "verified", ...caller });
      }
    } catch (error) {
      outcome = "error";
      return err(new AuthError({ reason: "invalid_signature" }));
    }
  } finally {
    try {
      recordCatalogDuration(
        "trellis.auth.verification.duration",
        performance.now() - startedAt,
        {
          "trellis.purpose": args.kind,
          "trellis.outcome": outcome,
        },
      );
    } catch {
      // Optional telemetry must not replace the verifier's result.
    }
  }
}

/** Map one descriptor permission atom into the verifier's nested atom. */
export function toCatalogAction(
  permission: DescriptorPermissionAtom,
): CatalogActionIdentity {
  return {
    kind: permission.surfaceKind,
    name: permission.surfaceName,
    direction: permission.action,
  };
}
