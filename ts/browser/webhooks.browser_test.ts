/**
 * The Webhooks section of a service's Console page: an administrator sees
 * each endpoint's URL and how it checks requests, sees what recent requests
 * became, and disables an endpoint so its URL answers 503.
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { participants } from "../../integration/fixtures/runtime/packages/runtime-trellis/index.js";
import { withTrellisRuntime } from "../integration/_support/runtime.ts";
import {
  BROWSER_ADMIN,
  browserRuntimeOptions,
  completeConsoleEntry,
  launchProfile,
} from "./browser_test_support.ts";

const participant = participants.WebhookService.participant;

test("an administrator sees a service's webhook endpoints and their requests, and disables one", async () => {
  await withTrellisRuntime(async (runtime) => {
    await runtime.ensureAdmin();
    const key = await runtime.services.createInstance({
      name: "billing",
      deployment: "billing",
      contract: participant,
      config: {
        values: { partnerUser: "partner" },
        secrets: {
          signing: "signing-secret",
          hookToken: "right-token-value",
          partnerPassword: "partner-password",
        },
      },
    });
    const { endpoints } = await runtime.callAdminRpc("webhooksEndpointsList", {
      service: key.deploymentId,
    });
    const url = endpoints.find(
      (endpoint) => endpoint.endpoint === "token",
    )!.url;
    // Intake follows settings as they change; once it holds the token, a
    // wrong one is refused.
    await runtime.waitFor(async () => {
      const refused = await fetch(url, {
        method: "POST",
        body: "{}",
        headers: { "x-hook-token": "wrong-token-value" },
      });
      return refused.status === 401 ? true : undefined;
    });

    const admin = await launchProfile(runtime);
    try {
      const page = await admin.newPage();
      await page.goto(
        `${runtime.trellisUrl}/console/admin/services/${key.deploymentId}`,
        { waitUntil: "domcontentloaded" },
      );
      await completeConsoleEntry(page, BROWSER_ADMIN);
      const token = page
        .getByRole("list", { name: "Webhook endpoints" })
        .getByRole("listitem", { name: "Token", exact: true });
      await token.getByText(url, { exact: true }).waitFor();
      await token.getByText("Token in the x-hook-token header").waitFor();
      const recent = token.getByRole("table", {
        name: "Recent requests to Token",
      });
      await recent.getByRole("cell", { name: "rejected" }).waitFor();
      assert.equal(await page.getByText("wrong-token-value").count(), 0);

      // A runtime setting changed here applies without a new build.
      await token
        .getByRole("button", { name: "Change Body size limit of Token" })
        .click();
      await token.getByLabel("Body size limit of Token").fill("1KiB");
      await token.getByRole("button", { name: "Save" }).click();
      await token.getByText("1 KiB", { exact: true }).waitFor();
      await runtime.waitFor(async () => {
        const response = await fetch(url, {
          method: "POST",
          body: "x".repeat(2048),
          headers: { "x-hook-token": "right-token-value" },
        });
        return response.status === 413 ? true : undefined;
      });

      // Text that is not a size, even one ending in a name every object
      // has, is refused with a message and saves nothing.
      await token
        .getByRole("button", { name: "Change Body size limit of Token" })
        .click();
      await token.getByLabel("Body size limit of Token").fill("5constructor");
      await token.getByRole("button", { name: "Save" }).click();
      await page
        .getByText("Body size limit takes a size, such as 512KiB or 16MiB.")
        .waitFor();
      await token.getByText("1 KiB", { exact: true }).waitFor();
      const { endpoints: described } = await runtime.callAdminRpc(
        "webhooksEndpointsList",
        { service: key.deploymentId },
      );
      assert.equal(
        described.find((endpoint) => endpoint.endpoint === "token")!.settings
          .maxBodyBytes,
        1024n,
      );

      await token.getByRole("button", { name: "Actions for Token" }).click();
      await token.getByRole("button", { name: "Disable webhook" }).click();
      await page
        .getByRole("dialog")
        .getByRole("button", { name: "Disable webhook" })
        .click();
      await token.getByText("Disabled", { exact: true }).waitFor();
      const disabled = await runtime.waitFor(async () => {
        const response = await fetch(url, {
          method: "POST",
          body: "{}",
          headers: { "x-hook-token": "right-token-value" },
        });
        return response.status === 503 ? response : undefined;
      });
      assert.equal(await disabled.text(), "");
    } finally {
      await admin.close();
    }
  }, browserRuntimeOptions());
});
