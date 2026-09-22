export type LoggerLike = {
  child(bindings: Record<string, unknown>): LoggerLike;
  trace(...args: unknown[]): void;
  debug(...args: unknown[]): void;
  info(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
};

function createNoopLogger(): LoggerLike {
  const noop = () => {};
  const logger: LoggerLike = {
    child: () => logger,
    trace: noop,
    debug: noop,
    info: noop,
    warn: noop,
    error: noop,
  };
  return logger;
}

// Keep the root package browser-safe: importing `@qlever-llc/trellis` must not
// pull in `pino` or Node-only side effects. Server-only entrypoints can inject
// a real logger, while the shared runtime defaults to a no-op logger here.
export const logger: LoggerLike = createNoopLogger();
