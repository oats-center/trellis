import { env } from "$env/dynamic/public";
import {
  createTrellisApp,
  type TrellisClientFor,
} from "@oatscenter/trellis-svelte";
import { apps } from "../../trellis/index.js";

export type TrellisDemoAppClient = TrellisClientFor<typeof apps.App.request>;

const defaultTrellisUrl = "http://localhost:3000";

export const trellisUrl = new URL(
  env.PUBLIC_TRELLIS_URL?.trim() || defaultTrellisUrl,
)
  .toString()
  .replace(/\/$/, "");

export const trellisApp = createTrellisApp({
  request: apps.App.request,
  trellisUrl,
});

/** Returns the generated browser app client from the current provider context. */
export function getTrellis(): TrellisDemoAppClient {
  return trellisApp.getTrellis();
}

/** Returns the current provider-owned transport connection. */
export function getConnection() {
  return trellisApp.getConnection();
}
