import { ulid } from "ulid";
import type { FieldOpsHandlerClient } from "../../deps.ts";

type ActivityInput = {
  kind: string;
  message: string;
  relatedSiteId?: string;
  relatedInspectionId?: string;
};

/** Publishes a compact activity event for demo workflows. */
export async function recordActivity(
  client: FieldOpsHandlerClient,
  activity: ActivityInput,
): Promise<void> {
  const occurredAt = new Date().toISOString();
  await client.publishAuditRecorded({
    activityId: `activity-${ulid()}`,
    occurredAt,
    ...activity,
  }).orThrow();
}
