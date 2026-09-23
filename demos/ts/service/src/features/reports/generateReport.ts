import type { OperationHandler } from "@oats-center/trellis/service";
import { BaseError } from "@oats-center/result";
import { isErr, UnexpectedError } from "@oats-center/trellis";
import { ASSIGNED_INSPECTIONS } from "../../../../shared/field_data.ts";
import { participants } from "../../../trellis/index.js";
import { recordActivity } from "../activity/index.ts";
import { buildReportRecord, recordReport } from "./reportStore.ts";

export const generateReport: OperationHandler<
  typeof participants.Service.participant,
  "Reports.Generate"
> = async ({ input, op, client }) => {
  function toBaseError(cause: unknown): BaseError {
    return cause instanceof BaseError ? cause : new UnexpectedError({ cause });
  }

  function isTerminalOperationError(error: BaseError): boolean {
    return error.message === "operation already terminal";
  }

  async function handleOperationError(
    cause: unknown,
  ): Promise<BaseError | null> {
    const error = toBaseError(cause);
    if (isTerminalOperationError(error)) {
      return null;
    }

    const failed = await op.fail(new UnexpectedError({ cause: error })).take();
    if (isErr(failed)) {
      if (isTerminalOperationError(failed.error)) {
        return null;
      }
      throw failed.error;
    }

    return error;
  }

  const inspection = ASSIGNED_INSPECTIONS.find((candidate) => {
    return candidate.inspectionId === input.inspectionId;
  });
  const inspectionLabel = inspection
    ? `${inspection.siteName} / ${inspection.assetName}`
    : input.inspectionId;
  const reportId = `closeout-${input.inspectionId}`;

  const started = await op.started().take();
  if (isErr(started)) {
    const error = await handleOperationError(started.error);
    if (!error) return;
    throw error;
  }
  await new Promise((resolve) => setTimeout(resolve, 900));

  for (
    const progress of [
      {
        stage: "drafting",
        message: `Collecting field notes for ${inspectionLabel}`,
      },
      { stage: "checking", message: "Checking evidence and site readiness" },
      {
        stage: "publishing",
        message: `Publishing closeout status for ${inspectionLabel}`,
      },
    ]
  ) {
    const updated = await op.progress(progress).take();
    if (isErr(updated)) {
      const error = await handleOperationError(updated.error);
      if (!error) return;
      throw error;
    }
    await new Promise((resolve) => setTimeout(resolve, 1_500));
  }

  const publishedAt = new Date().toISOString();
  const output = {
    reportId,
    inspectionId: input.inspectionId,
    status: "published",
  };
  const report = buildReportRecord({
    reportId,
    inspectionId: input.inspectionId,
    status: output.status,
    publishedAt,
    reportComment: input.reportComment,
  });

  const finalizing = await op.progress({
    stage: "finalizing",
    message: `Finalizing closeout report for ${inspectionLabel}`,
  }).take();
  if (isErr(finalizing)) {
    const error = await handleOperationError(finalizing.error);
    if (!error) return;
    throw error;
  }

  const reportsPublishedEvent = {
    reportId,
    inspectionId: input.inspectionId,
    siteId: inspection?.siteId,
    publishedAt,
  };
  await client.publishReportsPublished(reportsPublishedEvent).orThrow();
  recordReport(report);
  await recordActivity(client, {
    kind: "closeout-published",
    message: `Published closeout status for ${inspectionLabel}`,
    relatedSiteId: inspection?.siteId,
    relatedInspectionId: input.inspectionId,
  });

  const completed = await op.complete(output).take();
  if (isErr(completed)) {
    const error = await handleOperationError(completed.error);
    if (!error) return;
    throw error;
  }

  return completed;
};
