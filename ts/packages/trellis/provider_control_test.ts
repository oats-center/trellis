import type { ConnectedTrellisService } from "./service/runtime/service.ts";
import type { TrellisErrorInstance } from "./errors/index.ts";
import type {
  AuthError,
  DeviceUserAuthoritiesResolveOutput,
  DeviceUserAuthoritiesResolveProgress,
  DeviceUserAuthoritiesResolveUpdate,
  UnexpectedError,
  ValidationError,
} from "./internal_sdk/generated/apis/auth/mod.js";
import type { Participant } from "./internal_sdk/generated/participants/platform/mod.js";

type Equal<A, B> = (<T>() => T extends A ? 1 : 2) extends
  (<T>() => T extends B ? 1 : 2) ? true : false;
type Assert<T extends true> = T;

Deno.test("generated provider operation control preserves declared types", () => {
  async function check(
    provider: ConnectedTrellisService<Participant>,
    error: AuthError,
  ) {
    const registration = provider.handleDeviceUserAuthoritiesResolve;
    registration(async () => {});
    const handle = await registration.control("operation-id").orThrow();
    type Progress = Assert<
      Equal<
        Parameters<typeof handle.progress>[0],
        DeviceUserAuthoritiesResolveProgress
      >
    >;
    type Update = Assert<
      Equal<
        Parameters<typeof handle.emitUpdate>[0],
        DeviceUserAuthoritiesResolveUpdate
      >
    >;
    type Output = Assert<
      Equal<
        Parameters<typeof handle.complete>[0],
        DeviceUserAuthoritiesResolveOutput
      >
    >;
    type Failure = Assert<
      Equal<
        Parameters<typeof handle.fail>[0],
        AuthError | UnexpectedError | ValidationError | TrellisErrorInstance
      >
    >;
    const declaredError: Parameters<typeof handle.fail>[0] = error;
    void declaredError;
    type Checked = [Progress, Update, Output, Failure];
    const checked: Checked = [true, true, true, true];
    void checked;
    // @ts-expect-error progress requires the generated progress fields
    handle.progress({ state: "waiting" });
    // @ts-expect-error completion requires the generated output fields
    handle.complete({ value: "wrong" });
    // @ts-expect-error updates require the generated progress fields
    handle.emitUpdate({ state: "waiting" });
    // @ts-expect-error arbitrary Error is not a declared operation error
    handle.fail(new Error("wrong"));
  }
  void check;
});
