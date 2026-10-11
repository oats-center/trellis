import config from "../../../package.json" with { type: "json" };

/** Select runtime ranges from workspace dependencies without shipping build tooling. */
export function runtimeDependencies(
  imports: readonly string[],
): Record<string, string> {
  const dependencies = new Map(Object.entries(config.devDependencies));
  return Object.fromEntries(
    imports.map((name) => {
      const packageName = name
        .split("/")
        .slice(0, name.startsWith("@") ? 2 : 1)
        .join("/");
      const version = dependencies.get(packageName);
      if (!version) {
        throw new Error(`Runtime dependency ${name} has no workspace version`);
      }
      return [packageName, version];
    }),
  );
}
