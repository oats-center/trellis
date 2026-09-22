import Type from "typebox";

export const InspectionAssignment = Type.Object({
  inspectionId: Type.String({ minLength: 1 }),
  siteId: Type.String({ minLength: 1 }),
  siteName: Type.String({ minLength: 1 }),
  assetName: Type.String({ minLength: 1 }),
  checklistName: Type.String({ minLength: 1 }),
  priority: Type.Union([
    Type.Literal("high"),
    Type.Literal("medium"),
    Type.Literal("low"),
  ]),
  scheduledFor: Type.String({ minLength: 1 }),
});
