type ProcessLike = {
  env?: Record<string, string | undefined>;
};

type EnvironmentGlobalThis = typeof globalThis & {
  process?: ProcessLike;
};

/** Reads an optional native environment variable without loading Node in browsers. */
export function getEnv(key: string): string | undefined {
  const environmentGlobal = globalThis as EnvironmentGlobalThis;
  try {
    return environmentGlobal.process?.env?.[key];
  } catch {
    return undefined;
  }
}
