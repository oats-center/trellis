import { assertEquals, assertRejects } from "@std/assert";

import {
  type AdminDeploymentContext,
  createDeployment,
} from "../src/admin/deployment.ts";
import type {
  AdminRpc,
  TrellisTestAdminRpcMethod,
} from "../src/admin/methods.ts";

Deno.test("concurrent deployment creation shares failure and permits retry", async () => {
  const failure = new Error("deployment creation failed");
  let attempts = 0;
  const context: AdminDeploymentContext = {
    defaultDeployment: "test",
    createdDeployments: new Map(),
    deploymentBindingRevisions: new Map(),
    deploymentIds: new Map(),
    installedParticipants: new Map(),
    pendingApprovals: new Map(),
    rpc: <M extends TrellisTestAdminRpcMethod>(
      method: M,
      _input: AdminRpc[M]["input"],
    ): Promise<AdminRpc[M]["output"]> => {
      if (method !== "authDeploymentsCreate") {
        return Promise.reject(new Error(`unexpected admin RPC ${method}`));
      }
      attempts += 1;
      if (attempts === 1) return Promise.reject(failure);
      return Promise.resolve(
        {
          deployment: {
            kind: "service",
            deploymentId: "deployment-1",
            namespaces: [],
          },
        } as AdminRpc[M]["output"],
      );
    },
  };

  const first = createDeployment(context);
  const second = createDeployment(context);
  const firstError = await assertRejects(() => first, Error, failure.message);
  const secondError = await assertRejects(() => second, Error, failure.message);
  assertEquals(firstError, failure);
  assertEquals(secondError, failure);
  assertEquals(attempts, 1);
  await createDeployment(context);
  assertEquals(attempts, 2);
});
