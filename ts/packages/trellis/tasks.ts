import { AsyncResult, type BaseError, Result } from "@qlever-llc/result";
import { logger, type LoggerLike } from "./globals.ts";

export class TrellisTasks {
  #tasks: Record<string, Promise<Result<void, BaseError>>> = {};

  #log: LoggerLike;

  constructor(opts: { log?: LoggerLike }) {
    this.#log = opts.log ?? logger;
  }

  /*
   * Add a task to the task queue
   */
  add<E extends BaseError>(name: string, task: AsyncResult<void, E>) {
    if (name in this.#tasks) {
      this.#log.error({ name }, "Task already running?");
      throw new Error(`Task ${name} already running`);
    }
    this.#log.debug({ name }, "Added task");
    this.#tasks[name] = task.then((r: Result<void, E>) => {
      if (Result.isErr(r)) {
        this.#log.error(r, "Task encountered a runtime error");
      }

      return r;
    });
  }

  wait(): AsyncResult<void, BaseError> {
    return AsyncResult.any(Object.values(this.#tasks));
  }
}
