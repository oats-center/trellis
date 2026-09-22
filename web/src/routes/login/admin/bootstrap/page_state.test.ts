import { assertEquals } from "@std/assert";

import {
  accountFlowProviderLoginUrl,
  adminBootstrapFlowId,
  completeAdminBootstrap,
  formatAdminBootstrapError,
} from "./page_state.ts";

Deno.test("administrator OIDC carries the Console browser flow binding", () => {
  assertEquals(
    accountFlowProviderLoginUrl(
      "https://trellis.example",
      "admin-token",
      "oidc",
      {
        browserFlowId: "flow_console",
        portalBindingDigest: "binding_digest",
      },
    ),
    "https://trellis.example/auth/account-flow/admin-token/login/oidc?browserFlowId=flow_console&portalBindingDigest=binding_digest",
  );
});

Deno.test("administrator completion carries the Console browser flow", async () => {
  let submitted: unknown;
  const result = await completeAdminBootstrap(
    "http://trellis.example",
    "admin-token",
    {
      username: "admin",
      password: "secret-password",
      name: "Admin",
      email: "",
      browserFlowId: "flow_console",
      portalBindingDigest: "binding_digest",
    },
    async (_input, init) => {
      submitted = JSON.parse(String(init?.body));
      return new Response(JSON.stringify({
        status: "updated",
        userId: "usr_admin",
        browserFlowId: "flow_console",
      }));
    },
  );

  assertEquals(result, {
    status: "updated",
    userId: "usr_admin",
    browserFlowId: "flow_console",
  });
  assertEquals(submitted, {
    username: "admin",
    password: "secret-password",
    name: "Admin",
    email: null,
    browserFlowId: "flow_console",
    portalBindingDigest: "binding_digest",
  });
});

Deno.test("adminBootstrapFlowId reads a non-empty flow id", () => {
  assertEquals(
    adminBootstrapFlowId(
      new URL(
        "https://auth.example.com/login/admin/bootstrap?flowId=flow-1",
      ),
    ),
    "flow-1",
  );
});

Deno.test("adminBootstrapFlowId treats missing and blank values as absent", () => {
  assertEquals(
    adminBootstrapFlowId(
      new URL("https://auth.example.com/login/admin/bootstrap"),
    ),
    null,
  );
  assertEquals(
    adminBootstrapFlowId(
      new URL(
        "https://auth.example.com/login/admin/bootstrap?flowId=%20",
      ),
    ),
    null,
  );
});

Deno.test("formatAdminBootstrapError maps known backend errors", () => {
  assertEquals(
    formatAdminBootstrapError({
      status: 410,
      error: "flow_expired",
      message: null,
    }),
    "This bootstrap request has expired. Start bootstrap again.",
  );
  assertEquals(
    formatAdminBootstrapError({
      status: 409,
      error: "local_identity_exists",
      message: null,
    }),
    "That username is already in use. Choose a different username.",
  );
});

Deno.test("formatAdminBootstrapError keeps raw fallback for unknown errors", () => {
  assertEquals(
    formatAdminBootstrapError({ status: 418, error: "teapot", message: null }),
    "Bootstrap failed (418): teapot",
  );
});

Deno.test("formatAdminBootstrapError keeps status fallback when error body is absent", () => {
  assertEquals(
    formatAdminBootstrapError({ status: 500, error: null, message: null }),
    "Bootstrap failed with status 500.",
  );
});

Deno.test("formatAdminBootstrapError displays safe validation messages", () => {
  assertEquals(
    formatAdminBootstrapError({
      status: 400,
      error: "password_unchanged",
      message: null,
    }),
    "New password must differ from the current password.",
  );
});
