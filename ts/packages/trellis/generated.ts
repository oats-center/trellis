/** Portable runtime support for generated Trellis packages. */
export { TrellisError } from "./errors/TrellisError.ts";
export {
  apiDescriptor,
  type Codec,
  codecs,
  participantDescriptor,
  type SerializableErrorData,
} from "./generated_support.ts";
export type {
  ParticipantJobsFromResources,
  ParticipantKvFromResources,
  RuntimeApiFromGenerated,
} from "./participant_runtime/participant.ts";
