import Type from "typebox";

export const SiteSummary = Type.Object({
  siteId: Type.String({ minLength: 1 }),
  siteName: Type.String({ minLength: 1 }),
  openInspections: Type.Integer({ minimum: 0 }),
  overdueInspections: Type.Integer({ minimum: 0 }),
  latestStatus: Type.String({ minLength: 1 }),
  lastReportAt: Type.String({ minLength: 1 }),
});

export const SitesGetRequest = Type.Object({
  siteId: Type.String({ minLength: 1 }),
});
export const SitesGetResponse = Type.Object({
  site: Type.Optional(SiteSummary),
});
export const SitesRefreshRequest = Type.Object({
  siteId: Type.String({ minLength: 1 }),
});
export const SitesRefreshProgress = Type.Object({
  stage: Type.String({ minLength: 1 }),
  message: Type.String({ minLength: 1 }),
});
export const SitesRefreshResponse = Type.Object({
  refreshId: Type.String({ minLength: 1 }),
  site: SiteSummary,
  status: Type.String({ minLength: 1 }),
});
export const SiteRefreshJobPayload = Type.Object({
  siteId: Type.String({ minLength: 1 }),
});
export const SiteRefreshJobResult = Type.Object({
  refreshId: Type.String({ minLength: 1 }),
  site: SiteSummary,
  status: Type.String({ minLength: 1 }),
});
export const SitesRefreshedEvent = Type.Object({
  refreshId: Type.String({ minLength: 1 }),
  site: SiteSummary,
  refreshedAt: Type.String({ minLength: 1 }),
});
