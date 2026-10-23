import { type RuntimeApi } from "../participant_runtime/api.ts";
import { type Codec } from "../generated.ts";
import { type StateRegistration } from "./state.ts";
import {
  type EventDesc,
  type InferSchemaType,
  type LiveDesc,
} from "../participant.ts";
import {
  TransportError,
  UnexpectedError,
  ValidationError,
  type TrellisErrorInstance,
  type TrellisErrorMap,
  type TrellisErrorName,
} from "../errors/index.ts";
import { type InferRuntimeRpcError } from "./local_authorization.ts";
import { RemoteError } from "../errors/RemoteError.ts";

export type OwnedApiFor<TContract> = TContract extends {
  implements: readonly unknown[];
}
  ? RuntimeApi
  : never;

export type ContractKvFor<TContract> = TContract extends {
  resources: infer TResources extends Readonly<Record<string, unknown>>;
}
  ? {
      [
        K in keyof TResources as TResources[K] extends { kind: "kv" }
          ? K
          : never
      ]: TResources[K] extends {
        codec: infer TCodec;
        availability: infer TAvailability;
      }
        ? {
            value: TCodec extends Codec<infer TValue> ? TValue : unknown;
            schema: TCodec;
            required: TAvailability extends "required" ? true : false;
          }
        : never;
    }
  : {};

export type ContractJobsFor<TContract> = TContract extends {
  resources: infer TResources extends Readonly<Record<string, unknown>>;
}
  ? {
      [
        K in keyof TResources as TResources[K] extends { kind: "job" }
          ? K
          : never
      ]: TResources[K] extends {
        payload: infer TPayload;
        result: infer TResult;
        update: infer TUpdate;
      }
        ? {
            payload: TPayload extends Codec<infer TValue> ? TValue : unknown;
            result: TResult extends Codec<infer TValue> ? TValue : void;
            update: TUpdate extends Codec<infer TValue> ? TValue : never;
          }
        : never;
    }
  : {};

export type RuntimeStateStoreShape = {
  kind: "value";
  value: unknown;
  codec: Codec<unknown>;
  version: number;
  migrations: Readonly<Record<number, Codec<unknown>>>;
  registration?: StateRegistration;
};

export type RuntimeStateStores = Record<string, RuntimeStateStoreShape>;

export type RuntimeStateStoresForContract<TContract> = TContract extends {
  states: infer TStates extends Readonly<Record<string, unknown>>;
}
  ? {
      [K in keyof TStates]: TStates[K] extends {
        codec: infer TCodec;
        version: infer TVersion;
        migrations: infer TMigrations;
      }
        ? {
            kind: "value";
            value: TCodec extends Codec<infer TValue> ? TValue : unknown;
            codec: TCodec;
            version: TVersion;
            migrations: TMigrations;
          }
        : never;
    }
  : TContract extends {
        resources: infer TResources extends Readonly<Record<string, unknown>>;
      }
    ? {
        [
          K in keyof TResources as TResources[K] extends { kind: "state" }
            ? K
            : never
        ]: TResources[K] extends { codec: infer TCodec }
          ? {
              kind: "value";
              value: TCodec extends Codec<infer TValue> ? TValue : unknown;
              codec: TCodec;
              version: TResources[K] extends { version: infer TVersion }
                ? TVersion
                : number;
              migrations: TResources[K] extends {
                migrations: infer TMigrations;
              }
                ? TMigrations
                : {};
            }
          : never;
      }
    : {};

export type TrellisApiFor<TContract> = OwnedApiFor<TContract>;

export type RpcMethodsOf<TA extends RuntimeApi> = TA["rpc"];

export type MethodsOf<TA extends RuntimeApi> = keyof RpcMethodsOf<TA> & string;

export type RpcMethodNameOf<TA extends RuntimeApi> = MethodsOf<TA>;

export type OperationsOf<TA extends RuntimeApi> = keyof TA["operations"] &
  string;

export type EventsOf<TA extends RuntimeApi> = keyof TA["events"] & string;

export type LivesOf<TA extends RuntimeApi> = keyof NonNullable<TA["lives"]> &
  string;

export type RpcMethodOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> = RpcMethodsOf<TA>[M];

export type MethodInputOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> =
  RpcMethodOf<TA, M> extends { input: infer TInput }
    ? InferSchemaType<TInput>
    : never;

export type RpcInputOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = MethodInputOf<TA, M>;

export type MethodOutputOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> =
  RpcMethodOf<TA, M> extends { output: infer TOutput }
    ? InferSchemaType<TOutput>
    : never;

export type RpcOutputOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = MethodOutputOf<TA, M>;

export type RpcRequestShapes<TA extends RuntimeApi> = {
  [M in keyof TA["rpc"] & string]: {
    input: MethodInputOf<TA, M>;
    output: MethodOutputOf<TA, M>;
  };
};

export type RequestMethodOf<TRequests> = keyof TRequests & string;

export type RequestInputOf<
  TRequests,
  M extends RequestMethodOf<TRequests>,
> = TRequests[M] extends { input: infer TInput } ? TInput : never;

export type RequestOutputOf<
  TRequests,
  M extends RequestMethodOf<TRequests>,
> = TRequests[M] extends { output: infer TOutput } ? TOutput : never;

export type RpcDescriptorOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> =
  RpcMethodOf<TA, M> extends {
    input: infer TInput;
    output: infer TOutput;
    errors?: infer TErrors;
    runtimeErrors?: infer TRuntimeErrors;
    declaredErrorTypes?: infer TDeclaredErrorTypes;
  }
    ? {
        input: TInput;
        output: TOutput;
        errors?: TErrors;
        runtimeErrors?: TRuntimeErrors;
        declaredErrorTypes?: TDeclaredErrorTypes;
      } & RpcMethodOf<TA, M>
    : never;

export type DeclaredBuiltinErrorOf<TNames> =
  TNames extends readonly (infer TName)[]
    ? TName extends TrellisErrorName
      ? TrellisErrorMap[TName]
      : never
    : never;

export type DeclaredRuntimeErrorOf<TRuntimeErrors> =
  TRuntimeErrors extends readonly (infer TRuntimeError)[]
    ? InferRuntimeRpcError<TRuntimeError>
    : never;

export type MethodDeclaredErrorOf<
  TA extends RuntimeApi,
  M extends keyof TA["rpc"] & string,
> =
  RpcDescriptorOf<TA, M> extends {
    errors?: infer TErrors;
    runtimeErrors?: infer TRuntimeErrors;
  }
    ? DeclaredBuiltinErrorOf<TErrors> | DeclaredRuntimeErrorOf<TRuntimeErrors>
    : never;

export type RequestErrorOf<TA extends RuntimeApi, M extends MethodsOf<TA>> =
  | MethodDeclaredErrorOf<TA, M>
  | RemoteError
  | TransportError
  | ValidationError
  | UnexpectedError;

export type HandlerErrorOf<TA extends RuntimeApi, M extends MethodsOf<TA>> =
  MethodDeclaredErrorOf<TA, M> | TrellisErrorInstance;

export type OperationDescriptorOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends {
  input: infer TInput;
  progress?: infer TProgress;
  output?: infer TOutput;
  errors?: infer TErrors;
  runtimeErrors?: infer TRuntimeErrors;
  declaredErrorTypes?: infer TDeclaredErrorTypes;
}
  ? {
      input: TInput;
      progress?: TProgress;
      output?: TOutput;
      errors?: TErrors;
      runtimeErrors?: TRuntimeErrors;
      declaredErrorTypes?: TDeclaredErrorTypes;
    } & TA["operations"][O]
  : never;

export type OperationDeclaredErrorOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> =
  OperationDescriptorOf<TA, O> extends {
    errors?: infer TErrors;
    runtimeErrors?: infer TRuntimeErrors;
  }
    ? DeclaredBuiltinErrorOf<TErrors> | DeclaredRuntimeErrorOf<TRuntimeErrors>
    : never;

export type OperationHandlerErrorOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = OperationDeclaredErrorOf<TA, O> | TrellisErrorInstance;

export type EventMessageOf<TA extends RuntimeApi, E extends EventsOf<TA>> =
  TA["events"][E] extends EventDesc<infer TEvent>
    ? InferSchemaType<TEvent>
    : never;

export type EventOf<
  TA extends RuntimeApi,
  E extends EventsOf<TA>,
> = EventMessageOf<TA, E>;

export type EventDescriptorOf<TA extends RuntimeApi, E extends EventsOf<TA>> =
  TA["events"][E] extends EventDesc<infer TEvent>
    ? EventDesc<TEvent> & TA["events"][E]
    : never;

export type EventPayloadOf<
  TA extends RuntimeApi,
  E extends EventsOf<TA>,
> = EventOf<TA, E> & Record<string, unknown>;

/** Runtime metadata assigned to every Trellis event message. */
export type TrellisEventHeader = Readonly<{
  /** Stable event id used for event identity and JetStream de-duplication. */
  id: string;
  /** Event creation time in ISO-8601 format. */
  time: string;
}>;

/** Event body plus Trellis runtime event metadata. */
export type TrellisEventMessage<
  TBody extends Record<string, unknown> = Record<string, unknown>,
> = Readonly<{
  /** User-authored contract event body. */
  body: Readonly<TBody>;
  /** Runtime metadata assigned by Trellis. */
  header: TrellisEventHeader;
}>;

/** A fully encoded event whose subject, payload, and headers are stable. */
export type PreparedTrellisEvent<
  TPayload extends Record<string, unknown> = Record<string, unknown>,
> = Readonly<{
  event: string;
  descriptorIdentity: string;
  subject: string;
  /** Runtime event metadata assigned when the event was prepared. */
  header: TrellisEventHeader;
  payload: Readonly<TPayload>;
  encodedPayload: string;
  headers: Readonly<Record<string, string>>;
}>;

export type LiveInputOf<TA extends RuntimeApi, F extends LivesOf<TA>> =
  NonNullable<TA["lives"]>[F] extends LiveDesc<infer TInput, infer _TEvent>
    ? InferSchemaType<TInput>
    : never;

export type LiveEventOf<TA extends RuntimeApi, F extends LivesOf<TA>> =
  NonNullable<TA["lives"]>[F] extends LiveDesc<infer _TInput, infer TEvent>
    ? InferSchemaType<TEvent>
    : never;

export type LiveDescriptorOf<TA extends RuntimeApi, F extends LivesOf<TA>> =
  NonNullable<TA["lives"]>[F] extends LiveDesc<infer TInput, infer TEvent>
    ? LiveDesc<TInput, TEvent> & NonNullable<TA["lives"]>[F]
    : never;

export type OperationInputOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { input: infer TInput }
  ? InferSchemaType<TInput>
  : never;

export type OperationProgressOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { progress?: infer TProgress }
  ? TProgress extends undefined
    ? unknown
    : InferSchemaType<NonNullable<TProgress>>
  : unknown;

/** Infers the transient update payload for an operation descriptor. */
export type OperationUpdateOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { update?: infer TUpdate }
  ? TUpdate extends undefined
    ? unknown
    : InferSchemaType<NonNullable<TUpdate>>
  : unknown;

export type OperationOutputOf<
  TA extends RuntimeApi,
  O extends OperationsOf<TA>,
> = TA["operations"][O] extends { output?: infer TOutput }
  ? TOutput extends undefined
    ? unknown
    : InferSchemaType<NonNullable<TOutput>>
  : unknown;

export type RpcRequestErrorOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = RequestErrorOf<TA, M>;

export type RpcHandlerErrorOf<
  TA extends RuntimeApi,
  M extends RpcMethodNameOf<TA>,
> = HandlerErrorOf<TA, M>;

export type EventName<TContract> = EventsOf<OwnedApiFor<TContract>>;

export type EventType<TContract, E extends EventName<TContract>> = EventOf<
  OwnedApiFor<TContract>,
  E
>;

export type EventPayload<
  TContract,
  E extends EventName<TContract>,
> = EventPayloadOf<OwnedApiFor<TContract>, E>;
