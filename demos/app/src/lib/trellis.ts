import { env } from "$env/dynamic/public";
import {
  createTrellisApp,
  type TrellisClientFor,
} from "@oats-center/trellis-svelte";
import { participants } from "../../trellis/index.js";

export type TrellisDemoAppClient = TrellisClientFor<
  typeof participants.App.participant
>;

const defaultTrellisUrl = "http://localhost:3000";

export const trellisUrl = new URL(
  env.PUBLIC_TRELLIS_URL?.trim() || defaultTrellisUrl,
)
  .toString()
  .replace(/\/$/, "");

export const trellisApp = createTrellisApp({
  participant: participants.App.participant,
  trellisUrl,
});

export function getTrellis(): TrellisDemoAppClient {
  return trellisApp.getTrellis();
}

export function getConnection() {
  return trellisApp.getConnection();
}
