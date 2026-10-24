import {
  createTrellisApp,
  type TrellisClientFor,
} from "@oatscenter/trellis-svelte";
import { request as consoleRequest } from "../../../ts/packages/trellis/internal_sdk/generated/apps/console/mod.js";
import { APP_CONFIG } from "./config.ts";

export type TrellisConsoleClient = TrellisClientFor<typeof consoleRequest>;

let selectedTrellisUrl: string | undefined = APP_CONFIG.authUrl;

/** Sets the Trellis URL selected for the console's provider connection. */
export function setSelectedTrellisUrl(trellisUrl: string | undefined): void {
  selectedTrellisUrl = trellisUrl;
}

export const trellisApp = createTrellisApp({
  request: consoleRequest,
  trellisUrl: () => selectedTrellisUrl,
});

export function getTrellis(): TrellisConsoleClient {
  return trellisApp.getTrellis();
}

export function getAuthenticatedUser(trellis: TrellisConsoleClient) {
  return trellis.usersResolve({}).orThrow();
}

export function getConnection() {
  return trellisApp.getConnection();
}
