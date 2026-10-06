import config from "../../deno.json" with { type: "json" };

/** Select npm runtime ranges from the import map without shipping build tooling. */
export function runtimeDependencies(
  imports: readonly (keyof typeof config.imports)[],
): Record<string, string> {
  return Object.fromEntries(imports.map((name) => {
    const specifier = config.imports[name];
    const match = /^npm:((?:@[^/]+\/)?[^@/]+)@([^/]+)/.exec(specifier);
    if (!match) {
      throw new Error(
        `Runtime dependency ${name} is not a versioned npm import`,
      );
    }
    return [match[1], match[2]];
  }));
}
