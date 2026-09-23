import { AsyncResult, BaseError, isErr } from "@oats-center/result";
import { type apis } from "trellis-web-generated";

export type JobInspection = apis.jobs.InspectOutput;

export type JobsDetailData = {
  available: boolean;
  message?: string;
  inspection?: JobInspection;
};

type JobsPageRpc = {
  listServices(
    input: apis.jobs.ListServicesInput,
  ): AsyncResult<apis.jobs.ListServicesOutput, BaseError>;
  queryJobs(
    filter: apis.jobs.QueryInput,
  ): AsyncResult<apis.jobs.QueryOutput, BaseError>;
  summarizeJobs(
    filter: apis.jobs.SummaryInput,
  ): AsyncResult<apis.jobs.SummaryOutput, BaseError>;
};

type JobsDetailRpc = {
  inspect(
    input: apis.jobs.InspectInput,
  ): AsyncResult<apis.jobs.InspectOutput, BaseError>;
};

type JobsActionRpc<TOutput> = {
  action(
    input: { id: string; reason?: string },
  ): AsyncResult<TOutput, BaseError>;
};

function unavailableResult<T extends { available: false; message: string }>(
  result: T,
): T {
  return result;
}

function normalizedJobsUnavailable(error: unknown): string | null {
  let message: string;
  if (error instanceof BaseError) {
    message = String(error.getContext().causeMessage ?? error.message);
  } else if (error instanceof Error) {
    message = error.message;
  } else {
    message = String(error);
  }

  if (message.toLowerCase().includes("permissions violation")) {
    return "Your current session is not approved for Jobs RPCs. Sign out and sign back in to refresh permissions.";
  }

  const normalizedMessage = message.toLowerCase();
  if (
    normalizedMessage.includes("no responders") ||
    normalizedMessage.includes("references inactive contract") ||
    normalizedMessage.includes("not currently reachable")
  ) {
    return "Jobs admin runtime is not currently reachable.";
  }

  return null;
}

function isJobsNotFound(error: unknown): boolean {
  return error instanceof BaseError && error.name === "NotFoundError";
}

async function takeOrThrow<T>(result: AsyncResult<T, BaseError>): Promise<T> {
  const value = await result.take();
  if (isErr(value)) {
    throw value.error;
  }
  return value;
}

/** Queries Jobs workbench data through the typed Jobs.Query RPC boundary. */
export function queryJobs(
  rpc: Pick<JobsPageRpc, "queryJobs">,
  filter: apis.jobs.QueryInput,
): AsyncResult<apis.jobs.QueryOutput, BaseError> {
  return rpc.queryJobs(filter);
}

/**
 * Loads only the job query page. Service discovery and summary are separate
 * reads: a failure in either must not discard a successful query.
 */
export async function loadJobsQueryPage(
  rpc: Pick<JobsPageRpc, "queryJobs">,
  filter: apis.jobs.QueryInput,
): Promise<
  | {
    available: true;
    jobs: apis.jobs.QueryOutput["items"];
    nextCursor?: string;
  }
  | { available: false; message: string }
> {
  try {
    const value = await takeOrThrow(queryJobs(rpc, filter));
    return {
      available: true,
      jobs: value.items,
      ...(value.page.nextCursor === undefined
        ? {}
        : { nextCursor: value.page.nextCursor }),
    };
  } catch (error) {
    const message = normalizedJobsUnavailable(error);
    if (message) return { available: false, message };
    throw error;
  }
}

/** Loads only the service discovery catalog with complete cursor traversal. */
export async function loadJobsServices(
  rpc: Pick<JobsPageRpc, "listServices">,
): Promise<
  | { available: true; services: apis.jobs.ListServicesOutput["items"] }
  | { available: false; message: string }
> {
  try {
    const items: apis.jobs.ListServicesOutput["items"] = [];
    const seenCursors = new Set<string>();
    let cursor: string | undefined;
    for (;;) {
      const value = await takeOrThrow(rpc.listServices({
        page: { ...(cursor ? { cursor } : {}), limit: 100 },
      }));
      items.push(...value.items);
      const nextCursor = value.page.nextCursor;
      if (!nextCursor) return { available: true, services: items };
      if (seenCursors.has(nextCursor)) {
        throw new Error("Jobs.ListServices returned a cursor cycle");
      }
      seenCursors.add(nextCursor);
      cursor = nextCursor;
    }
  } catch (error) {
    const message = normalizedJobsUnavailable(error);
    if (message) return { available: false, message };
    throw error;
  }
}

/**
 * Loads the job summary. The caller passes the supported common scope; the
 * focused state tab is deliberately not part of it, so each tab's summary is a
 * state ledger rather than a summary of itself.
 */
export async function loadJobsSummary(
  rpc: Pick<JobsPageRpc, "summarizeJobs">,
  filter: apis.jobs.QueryInput,
): Promise<
  | {
    available: true;
    groups: apis.jobs.SummaryOutput["groups"];
    stats: apis.jobs.SummaryOutput["stats"];
    count: bigint;
  }
  | { available: false; message: string }
> {
  try {
    const value = await takeOrThrow(rpc.summarizeJobs(filter));
    return {
      available: true,
      groups: value.groups,
      stats: value.stats,
      count: value.count,
    };
  } catch (error) {
    const message = normalizedJobsUnavailable(error);
    if (message) return { available: false, message };
    throw error;
  }
}

/** Loads a single job by globally addressable job id. */
export async function loadJobDetailData(
  rpc: JobsDetailRpc,
  id: string,
): Promise<JobsDetailData> {
  try {
    const value = await takeOrThrow(rpc.inspect({ id }));
    return { available: true, inspection: value };
  } catch (error) {
    const message = normalizedJobsUnavailable(error);
    if (message) {
      return unavailableResult({ available: false, message });
    }
    if (isJobsNotFound(error)) {
      return { available: true };
    }
    throw error;
  }
}

/** Cancels a cancellable job by id. */
export async function cancelJob(
  rpc: JobsActionRpc<apis.jobs.CancelOutput>,
  id: string,
): Promise<apis.jobs.CancelOutput> {
  return takeOrThrow(rpc.action({ id }));
}

/** Retries a failed job by id. */
export async function retryJob(
  rpc: JobsActionRpc<apis.jobs.RetryOutput>,
  id: string,
): Promise<apis.jobs.RetryOutput> {
  return takeOrThrow(rpc.action({ id }));
}

/** Replays a dead-lettered job by id. */
export async function replayDlqJob(
  rpc: JobsActionRpc<apis.jobs.ReplayDLQOutput>,
  id: string,
): Promise<apis.jobs.ReplayDLQOutput> {
  return takeOrThrow(rpc.action({ id }));
}

/** Dismisses a dead-lettered job by id. */
export async function dismissDlqJob(
  rpc: JobsActionRpc<apis.jobs.DismissDLQOutput>,
  id: string,
): Promise<apis.jobs.DismissDLQOutput> {
  return takeOrThrow(rpc.action({ id }));
}
