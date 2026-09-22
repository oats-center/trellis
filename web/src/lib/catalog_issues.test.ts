import { equal } from "node:assert/strict";

import {
  contractDependencyBlockLabel,
  contractDependencyProviderContract,
  contractDependencyRequiredThing,
  parseContractDependencyBlock,
} from "./catalog_issues.ts";

declare const Deno: {
  test(name: string, fn: () => void): void;
};

Deno.test("parseContractDependencyBlock reads nested missing RPC details", () => {
  const detail = parseContractDependencyBlock(
    "Active contract digest 'blocked-digest' for 'krishi.workspace@v1' has invalid active dependencies (Dependency 'sherpa' references missing rpc 'GetSampleForWorkspace' on 'sherpa@v1')",
  );

  equal(detail.alias, "sherpa");
  equal(detail.surfaceKind, "RPC");
  equal(detail.surfaceName, "GetSampleForWorkspace");
  equal(detail.providerContractId, "sherpa@v1");
});

Deno.test("dependency block helpers identify provider-side missing surfaces", () => {
  const issue = {
    issueId: "issue-1",
    kind: "invalid-active-contract-uses",
    contractId: "krishi.workspace@v1",
    message:
      "Dependency 'sherpa' references missing rpc 'GetSampleForWorkspace' on 'sherpa@v1'",
  };

  equal(contractDependencyBlockLabel(issue), "RPC GetSampleForWorkspace");
  equal(contractDependencyRequiredThing(issue), "RPC GetSampleForWorkspace");
  equal(contractDependencyProviderContract(issue), "sherpa@v1");
});

Deno.test("parseContractDependencyBlock reads missing feed details", () => {
  const detail = parseContractDependencyBlock(
    "Dependency 'shares' references missing feed 'Activity' on 'krishi.shares@v1'",
  );

  equal(detail.surfaceKind, "Feed");
  equal(detail.surfaceName, "Activity");
  equal(detail.providerContractId, "krishi.shares@v1");
});

Deno.test("parseContractDependencyBlock reads inactive provider contract", () => {
  const detail = parseContractDependencyBlock(
    "Active contract digest 'blocked-digest' for 'krishi.workspace@v1' has invalid active dependencies (Dependency references inactive contract 'sherpa@v1')",
  );

  equal(detail.providerContractId, "sherpa@v1");
});

Deno.test("parseContractDependencyBlock reads aliased unknown provider contract", () => {
  const detail = parseContractDependencyBlock(
    "Dependency 'sherpa' references unknown contract 'sherpa@v1'",
  );

  equal(detail.providerContractId, "sherpa@v1");
});
