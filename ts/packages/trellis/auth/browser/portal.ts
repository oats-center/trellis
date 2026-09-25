import { decodeTrellisHttpError } from "../http_error.ts";
import { sha256 } from "../utils.ts";
import { type PortalFlowState, PortalFlowStateSchema } from "./flow_types.ts";
import type { StaticDecode } from "typebox";
import { Type } from "typebox";
import { Value } from "typebox/value";

export type { PortalFlowState } from "./flow_types.ts";
export type ApprovalDecision = "approved" | "denied";
export type AuthConfig = {
  authUrl: string;
  /** Explicit portal origin for non-browser automation; browsers supply Origin. */
  portalOrigin?: string;
};
export type PortalBinding = { secret: string; digest: string };

const PORTAL_BINDING_HEADER = "trellis-portal-binding";
const PORTAL_BINDING_KEY_PREFIX = "trellis.portal-binding.v1:";

function base64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(
    /=+$/,
    "",
  );
}

function decodeBase64Url(value: string): Uint8Array | null {
  try {
    const base64 = value.replaceAll("-", "+").replaceAll("_", "/");
    const binary = atob(base64.padEnd(Math.ceil(base64.length / 4) * 4, "="));
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch {
    return null;
  }
}

/** Returns the portal browser's per-flow verifier, creating it when absent. */
export async function createPortalBinding(): Promise<PortalBinding> {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  return {
    secret: base64Url(bytes),
    digest: base64Url(await sha256(Uint8Array.from(bytes))),
  };
}

/** Returns the portal browser's per-flow verifier, creating it when absent. */
export async function getOrCreatePortalBinding(
  flowId: string,
  storage: Storage,
): Promise<PortalBinding> {
  const key = `${PORTAL_BINDING_KEY_PREFIX}${flowId}`;
  const stored = storage.getItem(key);
  const storedBytes = stored ? decodeBase64Url(stored) : null;
  let bytes: Uint8Array;
  let secret: string;
  if (stored && storedBytes?.length === 32) {
    bytes = storedBytes;
    secret = stored;
  } else {
    const created = await createPortalBinding();
    const decoded = decodeBase64Url(created.secret);
    if (!decoded) throw new Error("generated portal binding is invalid");
    bytes = decoded;
    secret = created.secret;
  }
  if (secret !== stored) storage.setItem(key, secret);
  return {
    secret,
    digest: base64Url(await sha256(Uint8Array.from(bytes))),
  };
}

function authBaseUrl(config: AuthConfig): string {
  return config.authUrl.replace(/\/$/, "");
}

const BrowserFlowWireProperties = {
  flowId: Type.String({ minLength: 1 }),
  expiresAt: Type.Integer(),
  state: Type.Union([
    Type.Literal("choose_provider"),
    Type.Literal("authenticated"),
    Type.Literal("approval_required"),
    Type.Literal("approval_denied"),
    Type.Literal("approved"),
    Type.Literal("consumed"),
    Type.Literal("expired"),
  ]),
  providers: Type.Array(Type.String({ minLength: 1 })),
  registrationEnabled: Type.Boolean(),
  federatedRegistrationEnabled: Type.Boolean(),
  consentView: Type.Object({
    participantId: Type.String({ minLength: 1 }),
    packageDigest: Type.String({ minLength: 1 }),
    installedRevision: Type.Integer({ minimum: 1 }),
    expectedGrantRevision: Type.Integer({ minimum: 0 }),
    capabilities: Type.Array(Type.Object({
      id: Type.String({ minLength: 1 }),
      title: Type.String(),
      description: Type.String(),
      consequence: Type.String(),
      consentDigest: Type.String({ minLength: 1 }),
      required: Type.Boolean(),
      eligible: Type.Boolean(),
      alreadyApproved: Type.Boolean(),
    })),
    resources: Type.Array(Type.Object({
      kind: Type.Union([
        Type.Literal("kv"),
        Type.Literal("state"),
        Type.Literal("store"),
      ]),
      name: Type.String({ minLength: 1 }),
      title: Type.String(),
      description: Type.String(),
      required: Type.Boolean(),
      requestedCommitment: Type.Record(Type.String(), Type.Unknown()),
      actual: Type.Optional(Type.Record(Type.String(), Type.Unknown())),
      change: Type.Union([
        Type.Literal("new"),
        Type.Literal("unchanged"),
        Type.Literal("reduced"),
        Type.Literal("expanded"),
        Type.Literal("incompatible"),
        Type.Literal("detached"),
      ]),
      eligible: Type.Boolean(),
      alreadyApproved: Type.Boolean(),
    })),
    companion: Type.Optional(Type.Unknown()),
    decisionDigest: Type.String({ minLength: 1 }),
  }),
  redirectTarget: Type.Optional(Type.Union([
    Type.Null(),
    Type.String({ minLength: 1 }),
  ])),
};
const BrowserFlowWireSchema = Type.Object(BrowserFlowWireProperties);
const PortalFlowWireSchema = Type.Object({
  ...BrowserFlowWireProperties,
  decisionDigest: Type.String({ minLength: 1 }),
  user: Type.Object({
    origin: Type.String({ minLength: 1 }),
    id: Type.String({ minLength: 1 }),
    name: Type.Optional(Type.String({ minLength: 1 })),
    email: Type.Optional(Type.String({ minLength: 1 })),
    image: Type.Optional(Type.String({ minLength: 1 })),
  }),
});
type BrowserFlowWire = StaticDecode<typeof BrowserFlowWireSchema>;
type PortalFlowWire = StaticDecode<typeof PortalFlowWireSchema>;

function approval(wire: BrowserFlowWire) {
  const capabilities: Record<
    string,
    { displayName: string; description: string }
  > = {};
  for (const capability of wire.consentView.capabilities) {
    capabilities[`capability:${capability.id}`] = {
      displayName: capability.title || capability.id,
      description: capability.description,
    };
  }
  for (const resource of wire.consentView.resources) {
    capabilities[`resource:${resource.kind}:${resource.name}`] = {
      displayName: resource.title || resource.name,
      description: resource.description,
    };
  }
  return {
    contractId: wire.consentView.participantId,
    contractDigest: wire.consentView.packageDigest,
    displayName: wire.consentView.participantId,
    description: "",
    capabilities,
  };
}

function portalState(wire: BrowserFlowWire | PortalFlowWire): PortalFlowState {
  const evidence = approval(wire);
  let state: unknown;
  if (wire.state === "choose_provider") {
    const federatedProviders = wire.providers.filter((id) => id !== "local")
      .map(
        (id) => ({ id, displayName: id }),
      );
    state = {
      status: "choose_provider",
      flowId: wire.flowId,
      providers: wire.providers.map((id) => ({ id, displayName: id })),
      app: {
        contractId: evidence.contractId,
        contractDigest: evidence.contractDigest,
        displayName: evidence.displayName,
        description: evidence.description,
      },
      registration: {
        localIdentity: { available: wire.registrationEnabled },
        federatedIdentity: {
          available: wire.federatedRegistrationEnabled,
          providers: federatedProviders,
        },
      },
    };
  } else if (wire.state === "authenticated") {
    state = { status: "processing", flowId: wire.flowId };
  } else if (wire.state === "approval_required") {
    if (!("user" in wire) || !("decisionDigest" in wire)) {
      throw new Error("Authenticated portal flow requires portal binding");
    }
    state = {
      status: "approval_required",
      flowId: wire.flowId,
      consentViewDigest: wire.decisionDigest,
      optionalBundles: [],
      user: wire.user,
      approval: evidence,
    };
  } else if (wire.state === "approval_denied") {
    state = {
      status: "approval_denied",
      flowId: wire.flowId,
      approval: evidence,
      ...(wire.redirectTarget ? { returnLocation: wire.redirectTarget } : {}),
    };
  } else if (wire.state === "approved" || wire.state === "consumed") {
    if (!wire.redirectTarget) {
      throw new Error("Completed portal flow has no redirect target");
    }
    const location = new URL(wire.redirectTarget);
    location.searchParams.set("flowId", wire.flowId);
    state = { status: "redirect", location: location.toString() };
  } else if (wire.state === "expired") {
    state = {
      status: "expired",
      ...(wire.redirectTarget ? { returnLocation: wire.redirectTarget } : {}),
    };
  }
  return Value.Parse(PortalFlowStateSchema, state) as PortalFlowState;
}

async function fetchBrowserFlowWire(
  config: AuthConfig,
  flowId: string,
): Promise<BrowserFlowWire> {
  const response = await fetch(
    `${authBaseUrl(config)}/auth/flow/${encodeURIComponent(flowId)}`,
  );
  if (!response.ok) {
    throw await decodeTrellisHttpError(response);
  }
  return Value.Parse(BrowserFlowWireSchema, await response.json());
}

async function fetchBoundPortalFlowWire(
  config: AuthConfig,
  flowId: string,
  binding: PortalBinding,
): Promise<PortalFlowWire> {
  const response = await fetch(
    `${authBaseUrl(config)}/auth/flow/${encodeURIComponent(flowId)}/portal`,
    {
      method: "POST",
      headers: {
        ...(config.portalOrigin ? { origin: config.portalOrigin } : {}),
        [PORTAL_BINDING_HEADER]: binding.secret,
      },
    },
  );
  if (!response.ok) throw await decodeTrellisHttpError(response);
  return Value.Parse(PortalFlowWireSchema, await response.json());
}

export function portalFlowIdFromUrl(url: URL): string | null {
  return url.searchParams.get("flowId");
}

export async function fetchPortalFlowState(
  config: AuthConfig,
  flowId: string,
  binding: PortalBinding,
): Promise<PortalFlowState> {
  const flow = await fetchBrowserFlowWire(config, flowId);
  if (flow.state !== "authenticated" && flow.state !== "approval_required") {
    return portalState(flow);
  }
  return portalState(await fetchBoundPortalFlowWire(config, flowId, binding));
}

export function portalProviderLoginUrl(
  config: AuthConfig,
  providerId: string,
  flowId: string,
  binding: PortalBinding,
): string {
  const base = `${authBaseUrl(config)}/auth/login/${
    encodeURIComponent(providerId)
  }`;
  const query = new URLSearchParams({
    flowId,
    portalBindingDigest: binding.digest,
  });
  return `${base}?${query}`;
}

export async function submitPortalApproval(
  config: AuthConfig,
  flowId: string,
  binding: PortalBinding,
  decision: ApprovalDecision,
): Promise<PortalFlowState> {
  const flow = await fetchBrowserFlowWire(config, flowId);
  const wire =
    flow.state === "authenticated" || flow.state === "approval_required"
      ? await fetchBoundPortalFlowWire(config, flowId, binding)
      : null;
  if (wire?.state !== "approval_required") {
    throw new Error("Portal flow is not awaiting approval");
  }
  const response = await fetch(
    `${authBaseUrl(config)}/auth/flow/${encodeURIComponent(flowId)}/approval`,
    {
      method: "POST",
      headers: {
        "content-type": "application/json",
        ...(config.portalOrigin ? { origin: config.portalOrigin } : {}),
        [PORTAL_BINDING_HEADER]: binding.secret,
      },
      body: JSON.stringify({
        decision: decision === "approved" ? "approve" : "reject",
        approval: decision === "approved"
          ? {
            mode: "capabilities",
            installedRevision: wire.consentView.installedRevision,
            expectedGrantRevision: wire.consentView.expectedGrantRevision,
            decisionDigest: wire.consentView.decisionDigest,
            approvedCapabilities: wire.consentView.capabilities.filter((item) =>
              item.required && item.eligible
            ).map((item) => ({
              id: item.id,
              consentDigest: item.consentDigest,
            })),
            approvedResources: wire.consentView.resources.filter((item) =>
              item.required && item.eligible
            ).map((item) => ({
              kind: item.kind,
              name: item.name,
              commitment: item.requestedCommitment,
            })),
            companionApproved: wire.consentView.companion != null,
          }
          : undefined,
      }),
    },
  );

  if (!response.ok) {
    throw await decodeTrellisHttpError(response);
  }

  return portalState(Value.Parse(PortalFlowWireSchema, await response.json()));
}

export function portalRedirectLocation(
  state: PortalFlowState | null,
): string | null {
  if (state?.status === "redirect") return state.location;
  if (state?.status === "approval_denied") return state.returnLocation ?? null;
  if (state?.status === "expired") return state.returnLocation ?? null;
  return null;
}
