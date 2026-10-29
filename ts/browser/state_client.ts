import {
  TrellisClient,
  type ConnectedTrellisClient,
  StateConflictError,
} from "../packages/trellis/index.ts";
import { apps } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";

let client: ConnectedTrellisClient<typeof apps.StateCaller.request>;
let state: typeof client.state.savedResource;
let observation: Awaited<ReturnType<typeof openWatch>>;
async function openWatch() {
  return (await client.state.savedResource.watch()).orThrow();
}

/** Connect the generated app through ordinary browser OAuth. */
export async function connect(issuer: string, clientId: string) {
  client = (
    await TrellisClient.connect({
      trellisUrl: issuer,
      request: apps.StateCaller.request,
      auth: {
        clientId,
        redirectTo: `${location.origin}/callback`,
      },
    })
  ).orThrow();
  state = client.state.savedResource;
}
/** Return the typed first-use failure after a real registration reply is lost. */
export async function firstUseError() {
  return await Promise.all([state.get(), state.get()]).then((results) =>
    results.map((result) => {
      if (result.isOk()) throw new Error("Expected lost registration response");
      return Reflect.get(result.error, "code");
    }),
  );
}
/** Obtain a fresh handle through the generated application facade. */
export function freshHandle() {
  state = client.state.savedResource;
}
/** Explicitly start a new registration attempt while an older one is unresolved. */
export async function register() {
  return (await state.register("0")).orThrow();
}
/** Read the generated typed value. */
export async function get() {
  return (await state.get()).orThrow() ?? null;
}
/** Create a typed value through State CAS. */
export async function create(value: string) {
  return (await client.state.savedResource.create({ value })).orThrow();
}
/** Replace at the caller's revision, retaining typed conflict values. */
export async function replace(revision: string, value: string) {
  const result = await client.state.savedResource.replace(revision, { value });
  if (result.isErr()) {
    if (!(result.error instanceof StateConflictError)) throw result.error;
    return { conflict: true, current: result.error.current };
  }
  return { conflict: false, value: result.orThrow() };
}
/** Open the generated certified Live observation. */
export async function watch() {
  observation = await openWatch();
}
/** Consume a decoded State snapshot. */
export async function next() {
  const item = await observation.next();
  if (item.done) throw new Error("watch ended");
  return item.value ?? null;
}
/** Delete the stored value with explicit CAS. */
export async function remove(revision: string) {
  (await client.state.savedResource.delete(revision)).orThrow();
}
/** Release the certified observation. */
export async function closeWatch() {
  (await observation.close()).orThrow();
}
