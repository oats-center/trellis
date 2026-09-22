import Type from "typebox";

export const ReportsGenerateRequest = Type.Object({
  inspectionId: Type.String({ minLength: 1 }),
  reportComment: Type.String({ minLength: 1, pattern: "\\S" }),
});

export const ReportsGenerateProgress = Type.Object({
  stage: Type.String({ minLength: 1 }),
  message: Type.String({ minLength: 1 }),
});

export const ReportsGenerateResponse = Type.Object({
  reportId: Type.String({ minLength: 1 }),
  inspectionId: Type.String({ minLength: 1 }),
  status: Type.String({ minLength: 1 }),
});

export const ReportRecord = Type.Object({
  reportId: Type.String({ minLength: 1 }),
  inspectionId: Type.String({ minLength: 1 }),
  siteId: Type.Optional(Type.String({ minLength: 1 })),
  siteName: Type.String({ minLength: 1 }),
  assetName: Type.String({ minLength: 1 }),
  status: Type.String({ minLength: 1 }),
  publishedAt: Type.String({ minLength: 1 }),
  reportComment: Type.String({ minLength: 1, pattern: "\\S" }),
  summary: Type.String({ minLength: 1 }),
  readiness: Type.String({ minLength: 1 }),
  evidenceStatus: Type.String({ minLength: 1 }),
});

export const ReportsPublishedEvent = Type.Object({
  reportId: Type.String({ minLength: 1 }),
  inspectionId: Type.String({ minLength: 1 }),
  siteId: Type.Optional(Type.String({ minLength: 1 })),
  publishedAt: Type.String({ minLength: 1 }),
});
