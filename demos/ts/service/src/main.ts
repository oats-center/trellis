import { isErr } from "@oatscenter/trellis";
import { TrellisService } from "@oatscenter/trellis/service";
import process from "node:process";
import chalk from "chalk";
import { getSiteSummary, SITE_SUMMARIES } from "../../shared/field_data.ts";
import { participants } from "../trellis/index.js";
import type { FieldOpsDeps } from "./deps.ts";
import * as features from "./features/index.ts";

async function main(): Promise<void> {
  const [trellisUrl, seed] = process.argv.slice(2);
  if (!trellisUrl || !seed || process.argv.length !== 4) {
    console.error("Usage: demo-service <trellisUrl> <seed>");
    process.exit(1);
  }

  const service = await TrellisService.connect({
    trellisUrl,
    participant: participants.Service.participant,
    name: "field-ops-demo-service",
    seed,
  }).orThrow();
  const deps: FieldOpsDeps = {
    transferIssuer: service,
    getSiteSummary,
    activityFeedEventNames: {
      auditRecorded: "Audit.Recorded",
      reportsPublished: "Reports.Published",
      evidenceUploaded: "Evidence.Uploaded",
      sitesRefreshed: "Sites.Refreshed",
    },
  };

  service.health.setInfo({
    version: "0.0.0",
    info: { demo: "field-ops" },
  });

  for (const summary of SITE_SUMMARIES) {
    if (isErr(await service.kv.siteSummaries.get(summary.siteId).take())) {
      await service.kv.siteSummaries.create(summary.siteId, summary).orThrow();
    }
  }

  service.health.add("field-data", async () => {
    const checks = await Promise.all(
      SITE_SUMMARIES.map((summary) =>
        service.kv.siteSummaries.get(summary.siteId).take()
      ),
    );
    const loadedSites = checks.filter((result) => !isErr(result)).length;

    return {
      status: loadedSites === SITE_SUMMARIES.length ? "ok" : "failed",
      summary: `${loadedSites}/${SITE_SUMMARIES.length} demo sites loaded`,
      info: {
        expectedSites: SITE_SUMMARIES.length,
        loadedSites,
      },
    };
  });

  service.jobs.refreshSiteSummary.handle(
    features.sites.createRefreshSiteSummaryHandler(deps),
  );

  await service.handleAssignmentsList(
    features.assignments.listAssignments,
  );
  await service.handleSitesList(features.sites.listSites);
  await service.handleSitesGet(features.sites.getSite);
  await service.handleEvidenceList(features.evidence.listEvidence);
  await service.handleEvidenceDownload(
    features.evidence.createDownloadEvidenceHandler(deps),
  );
  await service.handleEvidenceDelete(features.evidence.deleteEvidence);
  await service.handleReportsList(features.reports.listReports);
  await service.handleSitesRefresh(features.sites.refreshSite);
  await service.handleReportsGenerate(
    features.reports.generateReport,
  );
  await service.handleEvidenceUpload(
    features.evidence.uploadEvidence,
  );
  await service.handleAuditFeed(
    async ({ emit, signal }) => {
      const controller = new AbortController();
      const stop = () => {
        controller.abort();
      };
      signal.addEventListener("abort", stop, { once: true });

      try {
        await service.onAuditRecorded(
          ({ event }) => {
            return emit({
              name: deps.activityFeedEventNames.auditRecorded,
              event,
            });
          },
          {},
          { mode: "ephemeral", replay: "new", signal: controller.signal },
        ).orThrow();
        await service.onReportsPublished(
          ({ event }) => {
            return emit({
              name: deps.activityFeedEventNames.reportsPublished,
              event,
            });
          },
          {},
          { mode: "ephemeral", replay: "new", signal: controller.signal },
        ).orThrow();
        await service.onEvidenceUploaded(
          ({ event }) => {
            return emit({
              name: deps.activityFeedEventNames.evidenceUploaded,
              event,
            });
          },
          {},
          { mode: "ephemeral", replay: "new", signal: controller.signal },
        ).orThrow();
        await service.onSitesRefreshed(
          ({ event }) => {
            return emit({
              name: deps.activityFeedEventNames.sitesRefreshed,
              event,
            });
          },
          {},
          { mode: "ephemeral", replay: "new", signal: controller.signal },
        ).orThrow();

        await new Promise<void>((resolve) => {
          signal.addEventListener("abort", () => resolve(), { once: true });
        });
      } finally {
        signal.removeEventListener("abort", stop);
        controller.abort();
      }
    },
  );

  console.log(chalk.green.bold("== Field Ops demo service"));
  let shuttingDown = false;
  const shutdown = async () => {
    if (shuttingDown) {
      return;
    }

    shuttingDown = true;

    try {
      await service.stop();
      process.exit(0);
    } catch (error) {
      console.error(chalk.red.bold("Failed to stop Field Ops demo service"));
      console.error(error);
      process.exit(1);
    }
  };

  process.on("SIGINT", () => void shutdown());
  process.on("SIGTERM", () => void shutdown());

  try {
    await service.wait();
  } catch (error) {
    console.error(
      chalk.red.bold("Field Ops demo service stopped unexpectedly"),
    );
    console.error(error);
    process.exit(1);
  }
}

if (import.meta.main) {
  await main();
}
