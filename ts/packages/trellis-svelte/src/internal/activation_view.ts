import type {
  DeviceUserAuthoritiesResolveOutput,
  DeviceUserAuthoritiesResolveProgress,
} from "@oatscenter/trellis/auth";
import type { TerminalOperation } from "@oatscenter/trellis";

export type DeviceActivationView =
  | { mode: "sign_in_required"; flowId: string }
  | { mode: "ready"; flowId: string }
  | {
    mode: "pending_review";
    flowId: string;
    state: DeviceUserAuthoritiesResolveProgress["state"];
  }
  | {
    mode: "activated";
    flowId: string;
    instanceId: string;
    deploymentId: string;
    activatedAt: string;
  }
  | { mode: "rejected"; flowId: string; reason?: string }
  | { mode: "expired"; flowId: string; reason: string }
  | { mode: "invalid_flow"; reason: string; flowId?: string };

function isoString(value: string | number | bigint | Date): string {
  return value instanceof Date
    ? value.toISOString()
    : typeof value === "number" || typeof value === "bigint"
    ? new Date(Number(value)).toISOString()
    : value;
}

function errorReason(error: unknown): string | undefined {
  if (typeof error !== "object" || error === null || !("reason" in error)) {
    return undefined;
  }

  const reason = Reflect.get(error, "reason");
  return typeof reason === "string" ? reason : undefined;
}

/**
 * Whether a companion resubmission failed because the signed-in user is not
 * allowed to decide it, which leaves the activation to a reviewer. Malformed,
 * stale, and transport failures are not reviewer outcomes.
 */
export function isDeviceActivationReviewerRequiredFailure(
  error: unknown,
): boolean {
  const reason = errorReason(error);
  return (
    reason === "not_authorized" ||
    reason === "insufficient_permissions" ||
    reason === "forbidden"
  );
}

export function createDeviceActivationReadyView(
  flowId: string,
): DeviceActivationView {
  return { mode: "ready", flowId };
}

export function createDeviceActivationSignInRequiredView(
  flowId: string,
): DeviceActivationView {
  return { mode: "sign_in_required", flowId };
}

export function createInvalidDeviceActivationView(
  reason: string,
  flowId?: string,
): DeviceActivationView {
  return flowId
    ? { mode: "invalid_flow", reason, flowId }
    : { mode: "invalid_flow", reason };
}

export function mapDeviceActivationOutput(
  flowId: string,
  result: DeviceUserAuthoritiesResolveOutput,
): DeviceActivationView {
  if (result.review.state === "approved") {
    return {
      mode: "activated",
      flowId,
      instanceId: result.device.instanceId,
      deploymentId: result.device.deploymentId,
      activatedAt: isoString(
        result.review.decidedAt ?? result.device.updatedAt,
      ),
    };
  }

  return {
    mode: "rejected",
    flowId,
    ...(result.review.reason ? { reason: result.review.reason } : {}),
  };
}

export function mapDeviceActivationProgress(
  flowId: string,
  progress: DeviceUserAuthoritiesResolveProgress,
): DeviceActivationView {
  return {
    mode: "pending_review",
    flowId,
    state: progress.state,
  };
}

export function mapDeviceActivationFailure(
  flowId: string,
  error: unknown,
): DeviceActivationView | null {
  const reason = errorReason(error);

  if (reason === "device_activation_flow_not_found") {
    return createInvalidDeviceActivationView(
      "This activation link is no longer valid.",
      flowId,
    );
  }

  if (reason === "device_activation_flow_expired") {
    return {
      mode: "expired",
      flowId,
      reason:
        "The activation request expired. Start again from the auth service.",
    };
  }

  if (reason === "unknown_device") {
    return createInvalidDeviceActivationView(
      "This activation link no longer matches a known device.",
      flowId,
    );
  }

  if (reason === "device_deployment_not_found") {
    return createInvalidDeviceActivationView(
      "This device deployment is no longer available.",
      flowId,
    );
  }

  if (reason === "invalid_request") {
    return createInvalidDeviceActivationView(
      "Trellis rejected this activation request. Start again from the device.",
      flowId,
    );
  }

  if (
    reason === "device_activation_revoked"
  ) {
    return { mode: "rejected", flowId, reason };
  }

  return null;
}

export function mapDeviceActivationTerminal(
  flowId: string,
  terminal: TerminalOperation<unknown, DeviceUserAuthoritiesResolveOutput>,
): DeviceActivationView | null {
  if (terminal.state === "completed") {
    return terminal.output
      ? mapDeviceActivationOutput(flowId, terminal.output)
      : null;
  }

  return terminal.error
    ? mapDeviceActivationFailure(flowId, terminal.error)
    : null;
}
