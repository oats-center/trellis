import Type from "typebox";

export const EvidenceUploadRequest = Type.Object({
  key: Type.String({ minLength: 1 }),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  evidenceType: Type.String({ minLength: 1 }),
  metadata: Type.Optional(Type.Record(Type.String(), Type.String())),
});

export const EvidenceUploadProgress = Type.Object({
  stage: Type.String({ minLength: 1 }),
  message: Type.String({ minLength: 1 }),
});

export const EvidenceUploadResponse = Type.Object({
  evidenceId: Type.String({ minLength: 1 }),
  key: Type.String({ minLength: 1 }),
  size: Type.Integer({ minimum: 0 }),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  fileName: Type.Optional(Type.String({ minLength: 1 })),
  disposition: Type.String({ minLength: 1 }),
});

export const EvidenceRecord = Type.Object({
  evidenceId: Type.String({ minLength: 1 }),
  key: Type.String({ minLength: 1 }),
  size: Type.Integer({ minimum: 0 }),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  evidenceType: Type.String({ minLength: 1 }),
  fileName: Type.Optional(Type.String({ minLength: 1 })),
  uploadedAt: Type.String({ minLength: 1 }),
});

export const EvidenceDownloadRequest = Type.Object({
  key: Type.String({ minLength: 1 }),
});

export const EvidenceFileInfo = Type.Object({
  key: Type.String({ minLength: 1 }),
  size: Type.Integer({ minimum: 0 }),
  updatedAt: Type.String({ minLength: 1 }),
  digest: Type.Optional(Type.String({ minLength: 1 })),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  metadata: Type.Record(Type.String({ minLength: 1 }), Type.String()),
});

export const EvidenceDownloadGrant = Type.Object({
  type: Type.Literal("TransferGrant"),
  direction: Type.Literal("receive"),
  service: Type.String({ minLength: 1 }),
  sessionKey: Type.String({ minLength: 1 }),
  transferId: Type.String({ minLength: 1 }),
  subject: Type.String({ minLength: 1 }),
  expiresAt: Type.String({ minLength: 1 }),
  chunkBytes: Type.Integer({ minimum: 1 }),
  info: Type.Object({
    ...EvidenceFileInfo.properties,
    digest: Type.String({ minLength: 1 }),
  }),
});

export const EvidenceDownloadResponse = Type.Object({
  transfer: EvidenceDownloadGrant,
});

export const EvidenceDeleteRequest = Type.Object({
  key: Type.String({ minLength: 1 }),
});

export const EvidenceDeleteResponse = Type.Object({
  key: Type.String({ minLength: 1 }),
  deleted: Type.Boolean(),
});

export const EvidenceUploadedEvent = Type.Object({
  evidenceId: Type.String({ minLength: 1 }),
  key: Type.String({ minLength: 1 }),
  size: Type.Integer({ minimum: 0 }),
  contentType: Type.Optional(Type.String({ minLength: 1 })),
  fileName: Type.Optional(Type.String({ minLength: 1 })),
  evidenceType: Type.String({ minLength: 1 }),
  uploadedAt: Type.String({ minLength: 1 }),
});
