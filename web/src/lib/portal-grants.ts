/** Returns whether role mappings repeat an exact provider and role key. */
export function hasDuplicateRoleMapping(
  mappings: readonly { providerId: string; role: string }[],
): boolean {
  const keys = new Set<string>();
  return mappings.some((mapping) => {
    const key = JSON.stringify([mapping.providerId, mapping.role]);
    if (keys.has(key)) return true;
    keys.add(key);
    return false;
  });
}
