import type { StaticDecode } from "typebox";
import { Type } from "typebox";

const OpenObjectSchema = Type.Object({}, { additionalProperties: true });
const AppSchema = Type.Object({
  displayName: Type.String({ minLength: 1 }),
}, { additionalProperties: true });
const ApprovalSchema = Type.Object({
  contractId: Type.String({ minLength: 1 }),
  contractDigest: Type.String({ minLength: 1 }),
  displayName: Type.String({ minLength: 1 }),
  description: Type.String(),
  capabilities: Type.Record(
    Type.String(),
    Type.Object({
      displayName: Type.String({ minLength: 1 }),
      description: Type.String(),
    }),
  ),
});
const UserSchema = Type.Object({
  origin: Type.String({ minLength: 1 }),
  id: Type.String({ minLength: 1 }),
  name: Type.Optional(Type.String({ minLength: 1 })),
  email: Type.Optional(Type.String({ minLength: 1 })),
  image: Type.Optional(Type.String({ minLength: 1 })),
});

export const PortalFlowStateSchema = Type.Union([
  Type.Object({
    status: Type.Literal("choose_provider"),
    flowId: Type.String({ minLength: 1 }),
    providers: Type.Array(Type.Object({
      id: Type.String({ minLength: 1 }),
      displayName: Type.String({ minLength: 1 }),
    })),
    app: AppSchema,
    portal: Type.Optional(OpenObjectSchema),
    registration: Type.Optional(OpenObjectSchema),
  }),
  Type.Object({
    status: Type.Literal("processing"),
    flowId: Type.String({ minLength: 1 }),
  }),
  Type.Object({
    status: Type.Literal("approval_required"),
    flowId: Type.String({ minLength: 1 }),
    consentViewDigest: Type.String({ minLength: 1 }),
    optionalBundles: Type.Array(OpenObjectSchema),
    user: UserSchema,
    approval: ApprovalSchema,
  }),
  Type.Object({
    status: Type.Literal("approval_denied"),
    flowId: Type.String({ minLength: 1 }),
    approval: ApprovalSchema,
    returnLocation: Type.Optional(Type.String({ minLength: 1 })),
  }),
  Type.Object({
    status: Type.Literal("insufficient_capabilities"),
    flowId: Type.String({ minLength: 1 }),
    user: Type.Optional(UserSchema),
    approval: ApprovalSchema,
    missingCapabilities: Type.Array(Type.String()),
    userCapabilities: Type.Array(Type.String()),
    returnLocation: Type.Optional(Type.String({ minLength: 1 })),
  }),
  Type.Object({
    status: Type.Literal("redirect"),
    location: Type.String({ minLength: 1 }),
  }),
  Type.Object({
    status: Type.Literal("expired"),
    returnLocation: Type.Optional(Type.String({ minLength: 1 })),
  }),
]);

export type PortalFlowState = StaticDecode<typeof PortalFlowStateSchema>;
