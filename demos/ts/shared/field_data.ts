export type InspectionPriority = "high" | "medium" | "low";

export type InspectionAssignment = {
  inspectionId: string;
  siteId: string;
  siteName: string;
  assetName: string;
  checklistName: string;
  priority: InspectionPriority;
  scheduledFor: string;
};

export type SiteSummary = {
  siteId: string;
  siteName: string;
  openInspections: number;
  overdueInspections: number;
  latestStatus: string;
  lastReportAt: string;
};

export const ASSIGNED_INSPECTIONS: InspectionAssignment[] = [
  {
    inspectionId: "insp-west-001",
    siteId: "site-west-yard",
    siteName: "West Yard",
    assetName: "Pump Station 7",
    checklistName: "Leak and vibration check",
    priority: "high",
    scheduledFor: "2026-04-18T09:00:00.000Z",
  },
  {
    inspectionId: "insp-ridge-002",
    siteId: "site-ridge-line",
    siteName: "Ridge Line",
    assetName: "Backup Generator 2",
    checklistName: "Run test and battery review",
    priority: "medium",
    scheduledFor: "2026-04-18T13:30:00.000Z",
  },
  {
    inspectionId: "insp-harbor-003",
    siteId: "site-harbor-gate",
    siteName: "Harbor Gate",
    assetName: "Security Gate Controller",
    checklistName: "Ingress log verification",
    priority: "low",
    scheduledFor: "2026-04-19T08:15:00.000Z",
  },
];

export const SITE_SUMMARIES: SiteSummary[] = [
  {
    siteId: "site-west-yard",
    siteName: "West Yard",
    openInspections: 3,
    overdueInspections: 1,
    latestStatus: "attention-needed",
    lastReportAt: "2026-04-17T18:12:00.000Z",
  },
  {
    siteId: "site-ridge-line",
    siteName: "Ridge Line",
    openInspections: 2,
    overdueInspections: 0,
    latestStatus: "on-track",
    lastReportAt: "2026-04-17T11:45:00.000Z",
  },
  {
    siteId: "site-harbor-gate",
    siteName: "Harbor Gate",
    openInspections: 1,
    overdueInspections: 0,
    latestStatus: "ready",
    lastReportAt: "2026-04-16T15:05:00.000Z",
  },
];

export function getSiteSummary(siteId: string): SiteSummary | undefined {
  return SITE_SUMMARIES.find((summary) => summary.siteId === siteId);
}
