import { errorMessage } from "./format";

const NETWORK_FAILURE =
  /timeout|timed out|websocket|refused|closed|network|connect/i;

function hostsOf(servers: unknown): string[] {
  if (!Array.isArray(servers)) return [];
  return servers
    .filter((server): server is string => typeof server === "string")
    .map((server) => {
      try {
        return new URL(server).host;
      } catch {
        return server;
      }
    });
}

/** One plain sentence saying why the browser could not connect. */
export function connectionProblem(error: unknown): string {
  const context =
    error &&
    typeof error === "object" &&
    "getContext" in error &&
    typeof error.getContext === "function"
      ? (error.getContext() as Record<string, unknown>)
      : {};
  const hosts = hostsOf(context.servers);
  const cause =
    typeof context.causeMessage === "string" ? context.causeMessage : "";
  if (hosts.length > 0 && NETWORK_FAILURE.test(cause)) {
    return `This browser couldn't reach Trellis at ${hosts.join(" or ")}. Check that this device can reach that address and trusts its certificate.`;
  }
  return errorMessage(error);
}
