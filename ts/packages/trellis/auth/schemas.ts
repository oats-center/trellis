import type { StaticDecode } from "typebox";
import { Type } from "typebox";

export const ContractDigestSchema = Type.String({
  pattern: "^[A-Za-z0-9_-]+$",
});

export const ClientTransportEndpointsSchema = Type.Object({
  natsServers: Type.Array(Type.String({ minLength: 1 }), { minItems: 1 }),
});

export type ClientTransportEndpoints = StaticDecode<
  typeof ClientTransportEndpointsSchema
>;

export const ClientTransportsSchema = Type.Object({
  native: Type.Optional(ClientTransportEndpointsSchema),
  websocket: Type.Optional(ClientTransportEndpointsSchema),
});

export type ClientTransports = StaticDecode<typeof ClientTransportsSchema>;

export const ApprovalDecisionSchema = Type.Union([
  Type.Literal("approved"),
  Type.Literal("denied"),
]);

export type ApprovalDecision = StaticDecode<typeof ApprovalDecisionSchema>;

export const UserParticipantKindSchema = Type.Union([
  Type.Literal("app"),
  Type.Literal("agent"),
]);

export type UserParticipantKind = StaticDecode<
  typeof UserParticipantKindSchema
>;

export const ContractApprovalCapabilitySchema = Type.Object({
  displayName: Type.String(),
  description: Type.String(),
  consequence: Type.Optional(Type.String()),
});

export type ContractApprovalCapability = StaticDecode<
  typeof ContractApprovalCapabilitySchema
>;

export const ContractApprovalSchema = Type.Object({
  contractDigest: ContractDigestSchema,
  contractId: Type.String(),
  displayName: Type.String(),
  description: Type.String(),
  participantKind: UserParticipantKindSchema,
  capabilities: Type.Record(Type.String(), ContractApprovalCapabilitySchema),
});

export type ContractApproval = StaticDecode<typeof ContractApprovalSchema>;

/** Returns the raw global capability keys required by an approval. */
export function approvalCapabilityKeys(approval: ContractApproval): string[] {
  return Object.keys(approval.capabilities);
}
