/**
 * How the Console names one instance of a service or device deployment.
 *
 * Provisioning often titles every instance after its deployment (the CLI uses
 * the name it was given), so a title alone may not tell replicas apart. A
 * title counts only when no sibling shares it and it is not just the
 * deployment's own name or ID; otherwise the instance is named by its ID.
 */

type InstanceLike = {
  instanceId: string;
  deploymentId: string;
  principal: { title: string };
  identityPublicKey?: string;
};

/** The parts the Console shows to identify one instance. */
export type InstanceIdentity = {
  /** The name to lead with: a distinct title, or a short form of the ID. */
  name: string;
  /** Whether `name` is the instance's own title. */
  titled: boolean;
  /** The first characters of the identity public key, when it has one. */
  key?: string;
};

/**
 * Names each instance so that siblings of one deployment never read alike.
 * `deploymentNames` holds each deployment's title, keyed by deployment ID;
 * `noun` leads an untitled instance's name ("Device 4KQ2ZD").
 */
export function instanceIdentities(
  instances: readonly InstanceLike[],
  deploymentNames: ReadonlyMap<string, string> = new Map(),
  noun = "Instance",
): Map<string, InstanceIdentity> {
  const counts = new Map<string, number>();
  const titleKey = (item: InstanceLike) =>
    `${item.deploymentId}\u0000${item.principal.title.trim().toLowerCase()}`;
  for (const item of instances)
    counts.set(titleKey(item), (counts.get(titleKey(item)) ?? 0) + 1);
  return new Map(
    instances.map((item) => {
      const title = item.principal.title.trim();
      const lowered = title.toLowerCase();
      const titled =
        title !== "" &&
        counts.get(titleKey(item)) === 1 &&
        lowered !== item.instanceId.toLowerCase() &&
        lowered !== item.deploymentId.toLowerCase() &&
        lowered !==
          (deploymentNames.get(item.deploymentId) ?? "").trim().toLowerCase();
      return [
        item.instanceId,
        {
          name: titled ? title : `${noun} ${item.instanceId.slice(-6)}`,
          titled,
          ...(item.identityPublicKey
            ? { key: item.identityPublicKey.slice(0, 8) }
            : {}),
        },
      ];
    }),
  );
}
