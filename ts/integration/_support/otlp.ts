import { Root } from "protobufjs";
import schema from "./otlp_metrics_schema.json" with { type: "json" };

/** String attributes used by the ordinary OTLP exporter. */
export type Attributes = {
  key: string;
  value?: { stringValue?: string | null } | null;
}[];

/** Numeric points decoded from the official OTLP protobuf schema. */
export interface MetricPoint {
  attributes: Attributes;
  asInt?: number | null;
  asDouble?: number | null;
}

/** Counter and gauge projections consumed by runtime acceptance tests. */
export interface Metric {
  name: string;
  sum?: { dataPoints: MetricPoint[] } | null;
  gauge?: { dataPoints: MetricPoint[] } | null;
}

/** Export envelope decoded with repeated fields initialized as arrays. */
export interface MetricExport {
  resourceMetrics: {
    resource?: { attributes?: Attributes } | null;
    scopeMetrics: {
      scope?: { name?: string | null } | null;
      metrics: Metric[];
    }[];
  }[];
}

const decoder = Root.fromJSON(schema).lookupType(
  "opentelemetry.proto.collector.metrics.v1.ExportMetricsServiceRequest",
);

/** Decode an actual exporter request using the official wire schema. */
export function decodeMetrics(bytes: Uint8Array): MetricExport {
  return decoder.toObject(decoder.decode(bytes), {
    arrays: true,
    longs: Number,
  }) as MetricExport;
}
