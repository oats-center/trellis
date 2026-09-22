import Type from "typebox";

export const AuditRecordedEvent = Type.Object({
  activityId: Type.String({ minLength: 1 }),
  kind: Type.String({ minLength: 1 }),
  message: Type.String({ minLength: 1 }),
  occurredAt: Type.String({ minLength: 1 }),
  relatedSiteId: Type.Optional(Type.String({ minLength: 1 })),
  relatedInspectionId: Type.Optional(Type.String({ minLength: 1 })),
});
