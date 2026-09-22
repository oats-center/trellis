import { APP_CONFIG } from "./config.ts";
import {
  getConnection,
  getTrellis,
  type TrellisConsoleClient,
} from "./trellis-context.svelte.ts";
export { getAuthenticatedUser } from "./trellis-context.svelte.ts";

export { getConnection, getTrellis };

export type AppTrellis = TrellisConsoleClient;
export type ConnectionStatus = ReturnType<typeof getConnection>["status"];

export const trellisUrl = APP_CONFIG.authUrl;
