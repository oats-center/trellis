import { ASSIGNED_INSPECTIONS } from "../../../../shared/field_data.ts";

export type ReportRecord = {
  reportId: string;
  inspectionId: string;
  siteId?: string;
  siteName: string;
  assetName: string;
  status: string;
  publishedAt: string;
  reportComment: string;
  summary: string;
  readiness: string;
  evidenceStatus: string;
};

const reports = new Map<string, ReportRecord>();

/** Records a completed demo report for later inspection through Reports.List. */
export function recordReport(report: ReportRecord): void {
  reports.set(report.reportId, report);
}

/** Lists completed demo reports newest first. */
export function listReports(): ReportRecord[] {
  const currentReports: ReportRecord[] = [];
  for (const [reportId, report] of reports) {
    if (
      typeof report.reportComment !== "string" ||
      report.reportComment.trim().length === 0
    ) {
      reports.delete(reportId);
      continue;
    }
    currentReports.push(report);
  }

  return currentReports.sort((left, right) => {
    return right.publishedAt.localeCompare(left.publishedAt);
  });
}

/** Builds the report document shown by the demo app after closeout. */
export function buildReportRecord(options: {
  reportId: string;
  inspectionId: string;
  status: string;
  publishedAt: string;
  reportComment: string;
}): ReportRecord {
  const inspection = ASSIGNED_INSPECTIONS.find((candidate) => {
    return candidate.inspectionId === options.inspectionId;
  });

  return {
    reportId: options.reportId,
    inspectionId: options.inspectionId,
    ...(inspection?.siteId ? { siteId: inspection.siteId } : {}),
    siteName: inspection?.siteName ?? "Unknown site",
    assetName: inspection?.assetName ?? "Unknown asset",
    status: options.status,
    publishedAt: options.publishedAt,
    reportComment: options.reportComment.trim(),
    summary: inspection
      ? `${inspection.checklistName} closeout for ${inspection.siteName}.`
      : `Closeout report for ${options.inspectionId}.`,
    readiness: "Site context reconciled before closeout.",
    evidenceStatus: "Evidence review completed in the inspection workflow.",
  };
}
