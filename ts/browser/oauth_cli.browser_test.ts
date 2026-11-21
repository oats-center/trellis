import assert from "node:assert/strict";
import { mkdir, writeFile } from "node:fs/promises";
import { test } from "node:test";
import { chromium } from "playwright";
import { browserTrustEnvironment } from "../packages/trellis-testkit/src/tls_trust.ts";
import { ulid } from "ulid";
import { codecs } from "@oatscenter/trellis/generated";
import { participants } from "../../integration/fixtures/client-upgrade/packages/client-upgrade/index.js";
import { participants as runtimeParticipants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import { TrellisService } from "../packages/trellis/service/mod.ts";
import {
  browserRuntimeOptions,
  loginCliAsAdministrator,
  startedServices,
} from "./browser_test_support.ts";
import { startDex } from "./dex_support.ts";

test(
  "CLI explicitly consents to administration, resumes custody, and manages clients, catalog revisions, service identities, Portal themes, and sign-in providers' pinned certificates",
  { timeout: 120_000 },
  async () => {
    await withTrellisRuntime(async (runtime) => {
      const browser = await chromium.launch({
        headless: true,
        env: browserTrustEnvironment(),
      });
      const page = await browser.newPage();
      page.setDefaultTimeout(10_000);
      const services = startedServices();
      try {
        const { directory, run } = await loginCliAsAdministrator(runtime, page);
        const clientId = `cli-development-${ulid()}`;
        const requestPath = `${directory}/client.json`;
        await writeFile(
          requestPath,
          JSON.stringify({
            clientId,
            expectedRevision: "0",
            title: clientId,
            description: "CLI browser development",
            kind: "browser",
            development: true,
            redirectUris: ["http://localhost:49761/callback"],
            impliedConsent: [],
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );
        const created = await run("clients", "put", requestPath);
        const loaded = await run("clients", "get", clientId);
        assert.equal(loaded.client.title, clientId);
        assert.equal(loaded.client.revision, created.client.revision);
        // The second process must resume saved OAuth custody; a stale mutation must
        // not overwrite the stored registration.
        await writeFile(
          requestPath,
          JSON.stringify({
            clientId,
            expectedRevision: "0",
            title: "Stale overwrite",
            description: "",
            kind: "browser",
            development: true,
            redirectUris: ["http://localhost:49761/callback"],
            impliedConsent: [],
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );
        await assert.rejects(run("clients", "put", requestPath));
        assert.equal(
          (await run("clients", "get", clientId)).client.title,
          clientId,
        );
        const disabled = await run(
          "clients",
          "disable",
          clientId,
          "--expected-revision",
          loaded.client.revision,
        );
        assert.equal(
          (await run("clients", "get", clientId)).client.enabled,
          false,
        );
        await run(
          "clients",
          "delete",
          clientId,
          "--expected-revision",
          disabled.revision,
        );
        await assert.rejects(run("clients", "get", clientId));
        const provider = participants.Provider.participant;
        const apiId = provider.implements[0].identity;
        const source = [
          "--source",
          new URL("../../integration/fixtures/client-upgrade", import.meta.url)
            .pathname,
          "--participant",
          provider.id,
        ];
        // Creating a deployment accepts the API it provides.
        const { reviews } = await run("apis", "review", ...source);
        assert.deepEqual(
          reviews.map((review: { apiId: string }) => review.apiId),
          [apiId],
        );
        await assert.rejects(run("apis", "get", apiId));
        const { deployment } = await run(
          "svc",
          "provider",
          "create",
          ...source,
        );
        const accepted = await run("apis", "get", apiId);
        assert.equal(accepted.api.canonicalDigest, reviews[0].canonicalDigest);
        // Accepting the same definition again changes nothing.
        const again = await run("apis", "accept", ...source);
        assert.equal(
          again.apis[0].acceptedRevision,
          accepted.api.acceptedRevision,
        );
        assert.equal(deployment.participantId, provider.id);
        // The service's owner makes its identity and hands over only the
        // public key: provisioning registers it and prints no seed, and the
        // service connects with the seed only the owner held.
        const owned = await run("keys", "new");
        const provisioned = await run(
          "svc",
          "provider",
          "provision",
          "--public-key",
          owned.publicKey,
        );
        assert.equal(provisioned.instanceSeed, null);
        assert.equal(provisioned.instance.identityPublicKey, owned.publicKey);
        const connected = services.add(
          TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: provider,
            seed: owned.seed,
            signal: services.signal,
          }).orThrow(),
        );
        await (await connected).stop();
        // The same key selects that instance again instead of adding one.
        const selected = await run(
          "svc",
          "provider",
          "provision",
          "--public-key",
          owned.publicKey,
        );
        assert.equal(selected.reused, true);
        assert.equal(
          selected.instance.instanceId,
          provisioned.instance.instanceId,
        );
        // A service that introduces itself does the same through a claim.
        const introduced = await run("keys", "new");
        const claimCreated = await run(
          "svc",
          "introduced",
          "create",
          "--public-key",
          introduced.publicKey,
        );
        assert.equal(claimCreated.instanceSeed, null);
        const introducing = services.add(
          TrellisService.connect({
            trellisUrl: runtime.trellisUrl,
            participant: runtimeParticipants.Provider.participant,
            seed: introduced.seed,
            signal: services.signal,
          }).orThrow(),
        );
        await runtime.waitFor(async () => {
          const { updates } = await run("svc", "introduced", "updates");
          return updates.find(
            (update: { state: string }) => update.state === "pending",
          );
        });
        assert.equal(
          (await run("svc", "introduced", "approve")).update.state,
          "applied",
        );
        const claimed = await introducing;
        await claimed.kv.records.put("added", { value: "cli" }).orThrow();
        await claimed.stop();
        // Settings: text as written, JSON otherwise, and a secret read from a
        // file that the CLI never shows back.
        await run(
          "svc",
          "configured",
          "create",
          "--source",
          new URL("../../integration/fixtures/runtime", import.meta.url)
            .pathname,
          "--participant",
          "localhost/runtime-trellis.ConfiguredService",
        );
        type Setting = { name: string; value?: unknown; isSet: boolean };
        const setting = (view: { fields: Setting[] }, name: string) =>
          view.fields.find((field) => field.name === name);
        const saved = await run(
          "svc",
          "configured",
          "config",
          "set",
          "endpoint=9090",
          "port=9090",
        );
        assert.equal(setting(saved, "endpoint")?.value, "9090");
        assert.equal(setting(saved, "port")?.value, 9090);
        const secretPath = `${directory}/api-key`;
        await writeFile(secretPath, "k1\n");
        const secret = setting(
          await run(
            "svc",
            "configured",
            "config",
            "set-secret",
            "apiKey",
            "--from-file",
            secretPath,
          ),
          "apiKey",
        );
        assert.equal(secret?.isSet, true);
        assert.equal(secret?.value, undefined);
        const { revisions } = await run(
          "svc",
          "configured",
          "config",
          "history",
        );
        assert.deepEqual(
          revisions.map(
            (revision: { revision: number; changedSecrets: string[] }) => [
              revision.revision,
              revision.changedSecrets,
            ],
          ),
          [
            [2, ["apiKey"]],
            [1, []],
          ],
        );
        // An optional secret can be stored as null without reading a value.
        const nulled = setting(
          await run(
            "svc",
            "configured",
            "config",
            "set-secret",
            "signingKey",
            "--null",
          ),
          "signingKey",
        );
        assert.equal(nulled?.isSet, true);
        // A nullable text setting stores null with --null, not the text "null".
        await run("svc", "configured", "config", "set", "region=eu");
        const cleared = setting(
          await run("svc", "configured", "config", "set", "--null", "region"),
          "region",
        );
        assert.equal(cleared?.isSet, true);
        assert.equal(cleared?.value, null);
        // An app's settings come from the IDL registered with its client.
        const appPath = `${directory}/configured-app.json`;
        await writeFile(
          appPath,
          JSON.stringify({
            clientId: "configured-app",
            expectedRevision: "0",
            title: "Configured app",
            description: "",
            kind: "native",
            development: true,
            redirectUris: ["http://127.0.0.1/callback"],
            impliedConsent: [],
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );
        const runtimeSource = new URL(
          "../../integration/fixtures/runtime",
          import.meta.url,
        ).pathname;
        const registered = await run(
          "clients",
          "put",
          appPath,
          "--source",
          runtimeSource,
          "--app",
          "localhost/runtime-trellis.ConfiguredAgent",
        );
        // A replacement whose source cannot be declared changes nothing.
        await writeFile(
          appPath,
          JSON.stringify({
            clientId: "configured-app",
            expectedRevision: registered.client.revision,
            title: "Replaced app",
            description: "",
            kind: "native",
            development: true,
            redirectUris: ["http://127.0.0.1/replaced"],
            impliedConsent: [],
            idempotencyKey: codecs.ulid.decode(ulid()),
          }),
        );
        await mkdir(`${directory}/broken`);
        await writeFile(`${directory}/broken/trellis.toml`, "[package\n");
        for (const source of [
          ["--source", `${directory}/missing`],
          ["--source", `${directory}/broken`],
          [
            "--source",
            runtimeSource,
            "--app",
            "localhost/runtime-trellis.Missing",
          ],
        ])
          await assert.rejects(run("clients", "put", appPath, ...source));
        const unchanged = (await run("clients", "get", "configured-app"))
          .client;
        assert.equal(unchanged.title, "Configured app");
        assert.equal(unchanged.revision, registered.client.revision);
        assert.deepEqual(unchanged.redirectUris, ["http://127.0.0.1/callback"]);
        const appSettings = await run(
          "clients",
          "config",
          "configured-app",
          "set",
          "limit=25",
        );
        assert.equal(setting(appSettings, "limit")?.value, 25);
        assert.equal(setting(appSettings, "greeting")?.value, "hello");
        const logo = `${directory}/logo.svg`;
        await writeFile(logo, '<svg xmlns="http://www.w3.org/2000/svg"/>');
        const themed = await run(
          "theme",
          "set",
          "--client",
          "trellis.console",
          "--name",
          "Acme Console",
          "--logo",
          logo,
          "--color",
          "primary-hover=#003a80",
          "--copy",
          "footer=Acme support",
        );
        assert.equal(themed.theme.colors.primaryHover, "#003a80");
        const shown = await run("theme", "show", "--client", "trellis.console");
        assert.equal(shown.theme.name, "Acme Console");
        assert.equal(shown.theme.logo.format, "svg");
        assert.equal(shown.theme.copy.footer, "Acme support");
        // An app theme cannot restyle the Portal for everyone.
        await assert.rejects(
          run(
            "theme",
            "set",
            "--client",
            "trellis.console",
            "--stylesheet",
            logo,
          ),
        );
        await run("theme", "remove", "--client", "trellis.console");
        assert.equal(
          (await run("theme", "show", "--client", "trellis.console")).theme,
          undefined,
        );
        // The server trusts only the system's certificates and Dex is served
        // from a private CA, so Auth reaches Dex only through the CLI's pin.
        // This catches the CLI dropping --certificate-fingerprint, an edit
        // that leaves it out losing the pin, and --no-certificate-fingerprint
        // not clearing it.
        const dex = await startDex({
          directory,
          redirectUri: `${runtime.trellisUrl}/portal/oidc/callback`,
          emails: [],
        });
        try {
          const add = (...flags: string[]) =>
            run(
              "sign-in",
              "providers",
              "add",
              "--name",
              "Company",
              "--issuer",
              dex.issuer,
              "--client-id",
              dex.clientId,
              ...flags,
            );
          const pinned = async () =>
            (await run("sign-in", "providers", "list")).items.map(
              (item: { certificateFingerprint?: string | null }) =>
                item.certificateFingerprint ?? null,
            );
          await assert.rejects(
            add(),
            // The CLI wraps its error text, so compare it without the wrapping.
            (error: Error) =>
              error.message
                .replace(/[\s│]/g, "")
                .includes(`issuer'sSHA-256fingerprintis${dex.fingerprint}`),
          );
          assert.deepEqual(await pinned(), []);
          const { provider } = await add(
            "--certificate-fingerprint",
            dex.fingerprint,
          );
          assert.equal(provider.certificateFingerprint, dex.fingerprint);
          const edit = (...flags: string[]) =>
            run("sign-in", "providers", "edit", provider.providerId, ...flags);
          const renamed = await edit("--name", "Company SSO");
          assert.equal(renamed.provider.title, "Company SSO");
          assert.deepEqual(await pinned(), [dex.fingerprint]);
          await edit("--no-certificate-fingerprint");
          assert.deepEqual(await pinned(), [null]);
          await edit("--certificate-fingerprint", dex.fingerprint);
          assert.deepEqual(await pinned(), [dex.fingerprint]);
        } finally {
          await dex.stop();
        }
        await run("logout");
        await assert.rejects(run("clients", "list"));
      } finally {
        // A failed step can leave a service waiting for approval.
        await services.stop();
        await browser.close();
      }
    }, browserRuntimeOptions());
  },
);
