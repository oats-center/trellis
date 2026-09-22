import { Type } from "typebox";

export const HealthInfoSchema = Type.Record(Type.String(), Type.Unknown());

export const HealthCheckResultSchema = Type.Object({
  name: Type.String(),
  status: Type.Union([Type.Literal("ok"), Type.Literal("failed")]),
  error: Type.Optional(Type.String()),
  summary: Type.Optional(Type.String()),
  info: Type.Optional(HealthInfoSchema),
  latencyMs: Type.Number(),
});

export const HealthResponseSchema = Type.Object({
  status: Type.Union([
    Type.Literal("healthy"),
    Type.Literal("unhealthy"),
    Type.Literal("degraded"),
  ]),
  service: Type.String(),
  timestamp: Type.String({ format: "date-time" }),
  checks: Type.Array(HealthCheckResultSchema),
});
