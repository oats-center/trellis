import { assert, assertEquals } from "@std/assert";

const docs = [
  "design/auth/trellis-auth.md",
  "design/auth/auth-api.md",
  "design/auth/auth-protocol.md",
  "design/auth/device-activation.md",
  "design/contracts/trellis-api-participants.md",
  "design/core/kv-resource-patterns.md",
  "design/jobs/trellis-jobs.md",
];

const repoRoot = new URL("../../../", import.meta.url);
const repoFile = (path: string) => new URL(path, repoRoot);

const retiredAuthorityPatterns = [
  /firstConnectPolicy/g,
  /compatibilityPolicy/g,
  /allowedDigests/g,
  /allowed-digest/gi,
  /applied contract digest set/gi,
  /active allowed set/gi,
  /appliedContracts\[\]\.allowedDigests/g,
  /InstanceGrantPolicy/g,
  /instance grant polic(?:y|ies)/gi,
  /portal profiles?/gi,
  /preActivationPolicy/g,
  /pre-activation device-owned/gi,
  /manual apply\/unapply/gi,
  /apply\/unapply flows/gi,
  /Auth\.Apply(?:Service|Device)DeploymentContract/g,
  /Auth\.(?:List|Set|Disable)PortalProfiles?/g,
  /Auth\.(?:List|Upsert|Disable)InstanceGrantPolic(?:y|ies)/g,
  /Auth\.(?:ValidateRequest|Me|GetDeviceConnectInfo|DecideDeviceActivationReview)/g,
  /Auth\.DeploymentAuthority\./g,
  /Auth\.IdentityAuthority\./g,
  /Auth\.IdentityGrants\./g,
  /known approved app\/agent contracts/gi,
  /known approved delegated contracts/gi,
  /\bapplied contract(?: digest)?\b/gi,
  /\binstalled contract(?:s| record| records| digest| digests)?\b/gi,
];

const requiredAuthorizationTerms = [
  "installed participant",
  "grantbinding",
  "platformprivilege::admin",
  "login session",
  "logical connection",
  "signed authorization context",
  "resource evidence",
  "grant set",
];

Deno.test("authorization docs define the GrantBinding vocabulary", async () => {
  const text = await Deno.readTextFile(repoFile("design/auth/trellis-auth.md"));

  for (const term of requiredAuthorizationTerms) {
    assert(
      text.toLowerCase().includes(term),
      `design/auth/trellis-auth.md should define '${term}'`,
    );
  }
});

Deno.test("auth API docs use installed participant and GrantBinding RPCs", async () => {
  const text = await Deno.readTextFile(repoFile("design/auth/auth-api.md"));

  for (
    const rpc of [
      "Auth.Deployments.Create",
      "Auth.Deployments.Apply",
      "Auth.Deployments.Get",
      "Auth.Participants.Get",
      "Auth.Participants.Install",
      "Auth.Grants.Get",
      "Auth.Grants.List",
      "Auth.Grants.Set",
      "Auth.Grants.Revoke",
      "Auth.Sessions.Me",
      "Auth.Connections.List",
    ]
  ) {
    assert(text.includes(rpc), `auth-api.md should document ${rpc}`);
  }
});

Deno.test("auth API section headings use grouped auth names", async () => {
  const text = await Deno.readTextFile(repoFile("design/auth/auth-api.md"));
  const ungroupedHeadings = text
    .split("\n")
    .filter((line) => line.startsWith("### rpc.Auth."))
    .filter((line) =>
      line.slice("### rpc.Auth.".length).includes(".") === false
    );

  assertEquals(ungroupedHeadings, []);
});

Deno.test("auth and contract docs do not endorse retired authority primitives", async () => {
  const failures: string[] = [];

  for (const doc of docs) {
    const text = await Deno.readTextFile(repoFile(doc));
    for (const pattern of retiredAuthorityPatterns) {
      const matches = text.match(pattern) ?? [];
      if (matches.length > 0) {
        failures.push(`${doc}: ${pattern} matched ${matches.length} time(s)`);
      }
    }
  }

  assertEquals(failures, []);
});
