import {
  TrellisClient,
  type ConnectedTrellisClient,
} from "../packages/trellis/index.ts";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

let client: ConnectedTrellisClient<typeof apps.StateCaller.request>;

/** Connect the generated browser app through ordinary browser OAuth. */
export async function connect(issuer: string, clientId: string) {
  client = (
    await TrellisClient.connect({
      trellisUrl: issuer,
      request: apps.StateCaller.request,
      auth: { clientId, redirectTo: `${location.origin}/callback` },
    })
  ).orThrow();
}

/** Read the app's State, which only the role it was given lets it do. */
export async function read() {
  return (await client.state.savedResource.get()).orThrow() ?? null;
}

/** Revoke the app's sign-in and end the Portal login, returning here. */
export async function logout() {
  await client.logout({
    endPortalLogin: true,
    returnTo: `${location.origin}/`,
  });
}
