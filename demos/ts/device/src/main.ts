import {
  checkDeviceActivation,
  TrellisDevice,
} from "@oatscenter/trellis/device";
import { TransportError } from "@oatscenter/trellis/errors";
import { TransferGrantSchema } from "@oatscenter/trellis";
import { Value } from "typebox/value";
import chalk from "chalk";
import { readFile, writeFile } from "node:fs/promises";
import process from "node:process";
import { createInterface } from "node:readline";
import { ulid } from "ulid";
import { participants } from "../trellis/index.js";
import { renderCompactQr } from "../../shared/compact_qr.ts";

const EVENT_WATCH_MS = 15_000;
const terminal = createInterface({
  input: process.stdin,
  output: process.stdout,
});

const inputLines = terminal[Symbol.asyncIterator]();

async function prompt(
  message: string,
  defaultValue = "",
): Promise<string | null> {
  process.stdout.write(
    `${message}${defaultValue ? ` [${defaultValue}]` : ""}: `,
  );
  const line = await inputLines.next();
  return line.done ? null : line.value || defaultValue;
}
const LIST_PAGE = { page: { limit: 50 } };

async function main(): Promise<void> {
  const [
    trellisUrl,
    rootSecret,
    provisioningSecret,
  ] = process.argv.slice(2);
  if (!trellisUrl || !rootSecret) {
    console.error(
      "Usage: deno task start <trellisUrl> <rootSecret> [provisioningSecret]",
    );
    process.exit(1);
  }

  const activation = await checkDeviceActivation({
    participant: participants.Device.participant,
    trellisUrl,
    rootSecret,
    ...(provisioningSecret === undefined ? {} : { provisioningSecret }),
  });

  if (activation.status === "not_ready") {
    throw new Error(`Device is not ready: ${activation.reason}`);
  }
  if (activation.status === "activation_required") {
    console.info("Please activate device at:", activation.activationUrl);
    renderCompactQr(activation.activationUrl);
    await activation.waitForOnlineApproval();
  }

  const device = await TrellisDevice.connect({
    participant: participants.Device.participant,
    trellisUrl,
    rootSecret,
  }).orThrow();

  try {
    console.log(chalk.green.bold("== Connected Field Device"));

    while (true) {
      printMenu();
      const selectedOption = await prompt("Select option");
      if (selectedOption === null) {
        return;
      }
      const choice = selectedOption.trim();

      switch (choice) {
        case "1":
          await listAssignments(device);
          break;
        case "2":
          await viewSelectedSite(device);
          break;
        case "3":
          await refreshSite(device);
          break;
        case "4":
          await generateReport(device);
          break;
        case "5":
          await uploadEvidence(device);
          break;
        case "6":
          await listAndDownloadEvidence(device);
          break;
        case "7":
          await watchActivity(device);
          break;
        case "8":
          await saveAndListDraftState(device);
          break;
        case "9":
          await runGuidedInspectionWizard(device);
          break;
        case "0":
          return;
        default:
          console.info("Choose a menu number from 0 through 9.");
      }
    }
  } finally {
    await device.connection.close();
  }
}

type Device = Awaited<ReturnType<typeof connectForTypes>>;

async function connectForTypes() {
  return await TrellisDevice.connect({
    participant: participants.Device.participant,
    trellisUrl: "http://localhost:0",
    rootSecret: "types-only",
  }).orThrow();
}

function printMenu(): void {
  console.log(chalk.cyan.bold("\nField Device Demo"));
  console.log("1. List assigned inspections");
  console.log("2. View selected site");
  console.log("3. Refresh site summary");
  console.log("4. Generate inspection report");
  console.log("5. Upload evidence file");
  console.log("6. List/download evidence files");
  console.log("7. Watch activity events briefly");
  console.log("8. Save/list draft state");
  console.log(chalk.bold("9. Guided inspection wizard"));
  console.log("0. Quit");
}

async function runGuidedInspectionWizard(device: Device): Promise<void> {
  console.log(chalk.green.bold("== Guided Inspection Wizard"));
  console.info("Step 1: choose an assigned inspection.");
  const assignments = (await device.assignmentsList(LIST_PAGE).orThrow()).items;
  if (assignments.length === 0) {
    console.info("No assignments are available for the guided workflow.");
    return;
  }

  assignments.forEach((assignment, index) => {
    console.info(
      `${
        index + 1
      }. ${assignment.inspectionId}: ${assignment.siteName} / ${assignment.assetName} (${assignment.priority})`,
    );
  });
  const selectedIndex =
    Number((await prompt("Inspection number", "1"))?.trim() || "1") - 1;
  const selected = assignments[selectedIndex] ?? assignments[0];

  await device.state.selectedSite.set({
    siteId: selected.siteId,
    siteName: selected.siteName,
    selectedAt: new Date().toISOString(),
  }).orThrow();

  console.info("Step 2: review and refresh the selected site.");
  const site = await device.sitesGet({ siteId: selected.siteId })
    .orThrow();
  if (site.site) printSite(site.site);
  await refreshSiteById(device, selected.siteId);

  console.info("Step 3: attach optional evidence.");
  const evidencePath = (await prompt("Evidence file path, or Enter to skip"))
    ?.trim();
  if (evidencePath) {
    await uploadEvidenceFile(device, evidencePath);
  }

  console.info("Step 4: generate the inspection report.");
  const reportComment = (await prompt("Report comment"))?.trim() ||
    "Guided field inspection completed.";
  await generateReportForInspection(
    device,
    selected.inspectionId,
    reportComment,
  );

  console.info("Step 5: save local draft notes.");
  const notes = (await prompt("Draft notes"))?.trim() ||
    "Guided workflow notes captured from the field device.";
  await device.kv.draftInspections.put(selected.inspectionId, {
    inspectionId: selected.inspectionId,
    siteId: selected.siteId,
    checklistName: selected.checklistName,
    notes,
    updatedAt: new Date().toISOString(),
  }).orThrow();

  console.info(chalk.green("Guided inspection workflow complete."));
}

function contentTypeForFile(fileName: string): string {
  const lower = fileName.toLowerCase();
  if (lower.endsWith(".jpg") || lower.endsWith(".jpeg")) return "image/jpeg";
  if (lower.endsWith(".png")) return "image/png";
  if (lower.endsWith(".gif")) return "image/gif";
  if (lower.endsWith(".webp")) return "image/webp";
  return "application/octet-stream";
}

function safeFileName(fileName: string): string {
  return fileName.replace(/[^a-zA-Z0-9._-]+/g, "-").replace(/^-+|-+$/g, "") ||
    "evidence.bin";
}

async function listAssignments(device: Device): Promise<void> {
  console.log(chalk.green.bold("== Assigned Inspections"));
  const result = await device.assignmentsList(LIST_PAGE).orThrow();

  if (result.items.length === 0) {
    console.info("No assigned inspections.");
    return;
  }

  for (const item of result.items) {
    console.info(
      `- ${item.inspectionId}: [${item.priority.toUpperCase()}] ${item.siteName} / ${item.assetName} (${item.checklistName}) at ${item.scheduledFor}`,
    );
  }
}

async function viewSelectedSite(device: Device): Promise<void> {
  console.log(chalk.green.bold("== Selected Site"));
  const selected = await device.state.selectedSite.get().orThrow();
  if (!selected) {
    console.info("No selected site saved. Use option 7 to save one.");
    return;
  }

  const result = await device.sitesGet({
    siteId: selected.value.siteId,
  }).orThrow();

  if (!result.site) {
    console.info(`Selected site ${selected.value.siteId} was not found.`);
    return;
  }

  printSite(result.site);
}

async function refreshSite(device: Device): Promise<void> {
  const siteId = (await prompt("Site ID to refresh"))?.trim();
  if (!siteId) {
    console.info("Refresh skipped: site ID is required.");
    return;
  }

  await refreshSiteById(device, siteId);
}

async function refreshSiteById(device: Device, siteId: string): Promise<void> {
  console.log(chalk.green.bold("== Refreshing Site Summary"));
  const operation = await device.sitesRefresh({ siteId })
    .start()
    .orThrow();
  console.info(`Accepted refresh operation ${operation.id}`);

  const events = await operation.watch().orThrow();
  for await (const event of events) {
    printOperationEvent(event);
    if (
      event.type === "completed" || event.type === "failed" ||
      event.type === "cancelled"
    ) {
      break;
    }
  }

  const terminal = await operation.wait().orThrow();
  console.info("Refresh finished:");
  console.dir(terminal.output, { depth: null });
}

async function generateReport(device: Device): Promise<void> {
  const inspectionId = (await prompt("Inspection ID"))?.trim();
  if (!inspectionId) {
    console.info("Report skipped: inspection ID is required.");
    return;
  }

  const reportComment = (await prompt("Report comment"))?.trim();
  if (!reportComment) {
    console.info("Report skipped: report comment is required.");
    return;
  }

  await generateReportForInspection(device, inspectionId, reportComment);
}

async function generateReportForInspection(
  device: Device,
  inspectionId: string,
  reportComment: string,
): Promise<void> {
  console.log(chalk.green.bold("== Generating Inspection Report"));
  const operation = await device.reportsGenerate({
    inspectionId,
    reportComment,
  })
    .start()
    .orThrow();
  console.info(`Accepted report operation ${operation.id}`);

  const events = await operation.watch().orThrow();
  for await (const event of events) {
    printOperationEvent(event);
    if (
      event.type === "completed" || event.type === "failed" ||
      event.type === "cancelled"
    ) {
      break;
    }
  }

  const terminal = await operation.wait().orThrow();
  console.info("Report operation finished:");
  console.dir(terminal.output, { depth: null });
}

async function uploadEvidence(device: Device): Promise<void> {
  const filePath = (await prompt("Evidence file path"))?.trim();
  if (!filePath) {
    console.info("Upload skipped: file path is required.");
    return;
  }

  await uploadEvidenceFile(device, filePath);
}

async function uploadEvidenceFile(
  device: Device,
  filePath: string,
): Promise<void> {
  const bytes = new Uint8Array(await readFile(filePath));
  const originalFileName = filePath.split(/[\\/]/).at(-1) || "evidence.bin";
  const evidenceId = ulid();
  const key = `evidence/${evidenceId}-${safeFileName(originalFileName)}`;
  let nextProgressPercent = 0;

  console.log(chalk.green.bold("== Uploading Evidence"));
  console.info(`Uploading ${bytes.length} bytes to ${key}`);

  const upload = await device.evidenceUpload({
    key,
    contentType: contentTypeForFile(originalFileName),
    evidenceType: "field-photo",
    metadata: {
      evidenceId,
      evidenceType: "field-photo",
      fileName: originalFileName,
    },
  })
    .transfer(bytes)
    .onTransfer((event: { transfer: { transferredBytes: number } }) => {
      const percent = Math.floor(
        event.transfer.transferredBytes / Math.max(bytes.length, 1) * 100,
      );
      if (percent >= nextProgressPercent) {
        console.info(
          `transfer ${percent}% (${event.transfer.transferredBytes}/${bytes.length} bytes)`,
        );
        nextProgressPercent = Math.min(100, percent + 10);
      }
    })
    .onProgress((event: { progress: { stage: string; message: string } }) => {
      console.info(
        `service ${event.progress.stage}: ${event.progress.message}`,
      );
    })
    .start()
    .orThrow();

  console.info(`Accepted upload operation ${upload.operation.id}`);
  const completed = await upload.wait().orThrow();
  console.info("Upload finished:");
  console.dir(completed.terminal.output, { depth: null });
}

async function listAndDownloadEvidence(device: Device): Promise<void> {
  console.log(chalk.green.bold("== Evidence Files"));
  const result = await device.evidenceList({
    ...LIST_PAGE,
    prefix: "evidence/",
  }).orThrow();
  if (result.items.length === 0) {
    console.info("No evidence files found.");
    return;
  }

  result.items.forEach((item, index) => {
    console.info(
      `${index + 1}. ${item.fileName ?? item.key} (${item.size} bytes, ${
        item.contentType ?? "unknown"
      })`,
    );
    console.info(`   key=${item.key}`);
  });

  const rawChoice = (await prompt("Download which number? Press Enter to skip"))
    ?.trim();
  if (!rawChoice) return;

  const choice = Number(rawChoice);
  const selected = Number.isInteger(choice)
    ? result.items[choice - 1]
    : undefined;
  if (!selected) {
    console.info("No evidence file selected.");
    return;
  }

  const defaultName = safeFileName(
    selected.fileName ?? selected.key.split("/").at(-1) ?? "evidence.bin",
  );
  const outputPath =
    (await prompt("Output path", `./${defaultName}`))?.trim() ||
    `./${defaultName}`;
  const download = await device.evidenceDownload({
    key: selected.key,
  }).orThrow();
  const transfer = Value.Parse(TransferGrantSchema, download.transfer);
  if (transfer.direction !== "receive") {
    throw new Error("Evidence download returned a send transfer grant.");
  }
  const downloaded = await device.transfer(transfer).bytes().orThrow();
  await writeFile(outputPath, downloaded);
  console.info(`Downloaded ${downloaded.byteLength} bytes to ${outputPath}`);
}

async function watchActivity(device: Device): Promise<void> {
  console.log(chalk.green.bold("== Watching Events"));
  console.info(
    `Watching new activity and report events for ${EVENT_WATCH_MS / 1000}s.`,
  );

  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), EVENT_WATCH_MS);

  try {
    await device.onAuditRecorded(
      (event) => {
        console.info("Audit.Recorded");
        console.dir(event, { depth: null });
      },
      { mode: "ephemeral", signal: controller.signal },
    ).orThrow();
    await device.onReportsPublished(
      (event) => {
        console.info("Reports.Published");
        console.dir(event, { depth: null });
      },
      { mode: "ephemeral", signal: controller.signal },
    ).orThrow();

    await new Promise((resolve) => setTimeout(resolve, EVENT_WATCH_MS));
  } finally {
    clearTimeout(timer);
    controller.abort();
  }
}

async function saveAndListDraftState(device: Device): Promise<void> {
  console.log(chalk.green.bold("== Draft State"));
  const assignments = (await device.assignmentsList(LIST_PAGE).orThrow()).items;
  const selected = assignments[0];
  if (!selected) {
    console.info("No assignments available for sample state.");
    return;
  }

  await device.state.selectedSite.set({
    siteId: selected.siteId,
    siteName: selected.siteName,
    selectedAt: new Date().toISOString(),
  }).orThrow();

  const notes = (await prompt("Draft notes"))?.trim() ||
    "Field notes captured from the consolidated device demo.";
  await device.kv.draftInspections.put(selected.inspectionId, {
    inspectionId: selected.inspectionId,
    siteId: selected.siteId,
    checklistName: selected.checklistName,
    notes,
    updatedAt: new Date().toISOString(),
  }).orThrow();

  const selectedSite = await device.state.selectedSite.get().orThrow();
  const draftKeys = await device.kv.draftInspections.keys().orThrow();
  const drafts = [];
  for await (const key of draftKeys) {
    const draft = await device.kv.draftInspections.get(key).orThrow();
    if (draft) drafts.push(draft);
    if (drafts.length === 10) break;
  }

  console.info("Selected site state:");
  console.dir(selectedSite, { depth: null });
  console.info("Draft inspection state:");
  console.dir(drafts, { depth: null });
}

function printSite(site: {
  siteId: string;
  siteName: string;
  openInspections: number;
  overdueInspections: number;
  latestStatus: string;
  lastReportAt: string;
}): void {
  console.info(
    `- ${site.siteName} (${site.siteId}): ${site.openInspections} open, ${site.overdueInspections} overdue, status ${site.latestStatus}, last report ${site.lastReportAt}`,
  );
}

function printOperationEvent(event: {
  type: string;
  progress?: { stage: string; message: string };
  snapshot: { state: string };
}): void {
  if (event.type === "progress" && event.progress) {
    console.info(`${event.progress.stage}: ${event.progress.message}`);
    return;
  }

  console.info(`${event.type}: ${event.snapshot.state}`);
}

if (import.meta.main) {
  try {
    await main();
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") {
      console.error(chalk.red.bold("File not found"));
      console.error(error.message);
      process.exit(1);
    }
    if (error instanceof TransportError) {
      console.error(chalk.red.bold("Trellis request failed"));
      console.error(`${error.message} (${error.code})`);
      console.error(error.hint);
      process.exit(1);
    }

    throw error;
  } finally {
    terminal.close();
  }
}
