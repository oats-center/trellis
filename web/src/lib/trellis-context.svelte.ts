import {
  createTrellisApp,
  type TrellisClientFor,
} from "@oats-center/trellis-svelte";
import { participant as consoleParticipant } from "../../../ts/packages/trellis/internal_sdk/generated/participants/console/mod.js";
import { APP_CONFIG } from "./config.ts";

export type TrellisConsoleClient = TrellisClientFor<
  typeof consoleParticipant
>;

let selectedTrellisUrl: string | undefined = APP_CONFIG.authUrl;

/** Sets the Trellis URL selected for the console's provider connection. */
export function setSelectedTrellisUrl(trellisUrl: string | undefined): void {
  selectedTrellisUrl = trellisUrl;
}

export const trellisApp = createTrellisApp({
  participant: consoleParticipant,
  trellisUrl: () => selectedTrellisUrl,
});

export function getTrellis(): TrellisConsoleClient {
  return trellisApp.getTrellis();
}

export function getAuthenticatedUser(trellis: TrellisConsoleClient) {
  return trellis.sessionsMe({}).orThrow();
}

export function getConnection() {
  return trellisApp.getConnection();
}
