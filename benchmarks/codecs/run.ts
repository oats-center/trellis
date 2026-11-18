import { writeFile } from "node:fs/promises";
import { hostname } from "node:os";
import { parseArgs } from "node:util";
import { codecs, type Codec } from "@oatscenter/trellis/generated";

const { values: args } = parseArgs({
  options: {
    output: { type: "string" },
    samples: { type: "string", default: "15" },
    "min-sample-ms": { type: "string", default: "50" },
  },
});
const samples = Number(args.samples);
const minSampleMs = Number(args["min-sample-ms"]);

// A narrow model, as in a large paginated list response.
const row = codecs.model({
  id: codecs.ulid,
  name: codecs.string,
  count: codecs.u32,
  note: codecs.optional(codecs.nullable(codecs.string)),
});
const rows = codecs.list(row);
const rowsWire = Array.from({ length: 10_000 }, (_, index) => ({
  id: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
  name: `row-${index}`,
  count: index,
  ...(index % 3 === 0 ? { note: null } : index % 3 === 1 ? { note: "n" } : {}),
}));

// A wide model: 48 known fields, a nested 16-field model, optional fields
// both absent and explicitly null, and fields from a newer contract version.
const nestedFields: Record<string, Codec<unknown>> = {};
for (let index = 0; index < 16; index++)
  nestedFields[`inner${index}`] = codecs.string;
const nested = codecs.model(nestedFields);
const wideFields: Record<
  string,
  Codec<unknown> | ReturnType<typeof codecs.optional<unknown>>
> = { nested };
for (let index = 0; index < 16; index++) {
  wideFields[`text${index}`] = codecs.string;
  wideFields[`number${index}`] = codecs.i32;
  wideFields[`maybe${index}`] = codecs.optional(codecs.nullable(codecs.string));
}
const wide = codecs.model(wideFields);
const wideWire: Record<string, unknown> = {
  nested: Object.fromEntries([
    ...Object.keys(nestedFields).map((name) => [name, name]),
    ["innerFuture", { kept: true }],
  ]),
};
for (let index = 0; index < 16; index++) {
  wideWire[`text${index}`] = `value-${index}`;
  wideWire[`number${index}`] = index;
  if (index % 2 === 0) wideWire[`maybe${index}`] = index % 4 ? null : "set";
}
for (let index = 0; index < 8; index++) wideWire[`future${index}`] = index;
const wides = codecs.list(wide);
const widesWire = Array.from({ length: 1_000 }, () => wideWire);

const boundedLength = 4096;
const bounded = codecs.bounded(codecs.string, {
  minCount: 1,
  maxCount: boundedLength,
});
const ascii = "a".repeat(boundedLength);
const astral = "\u{1F600}".repeat(boundedLength);

const rowsDecoded = rows.decode(rowsWire);
const widesDecoded = wides.decode(widesWire);
const cases: Record<string, () => unknown> = {
  "list-10000-narrow/decode": () => rows.decode(rowsWire),
  "list-10000-narrow/encode": () => rows.encode(rowsDecoded),
  "list-1000-wide/decode": () => wides.decode(widesWire),
  "list-1000-wide/encode": () => wides.encode(widesDecoded),
  "bounded-4096-ascii/decode": () => bounded.decode(ascii),
  "bounded-4096-astral/decode": () => bounded.decode(astral),
  "bounded-4096-astral/encode": () => bounded.encode(astral),
};

let sink: unknown;
const results: Record<string, { medianNs: number; minNs: number }> = {};
for (const [name, run] of Object.entries(cases)) {
  // Calibrate iterations so each sample runs long enough to time reliably.
  let iterations = 1;
  for (;;) {
    const start = performance.now();
    for (let index = 0; index < iterations; index++) sink = run();
    if (performance.now() - start >= minSampleMs) break;
    iterations *= 2;
  }
  const perOp: number[] = [];
  for (let sample = 0; sample < samples; sample++) {
    const start = performance.now();
    for (let index = 0; index < iterations; index++) sink = run();
    perOp.push(((performance.now() - start) * 1e6) / iterations);
  }
  perOp.sort((a, b) => a - b);
  results[name] = {
    medianNs: perOp[Math.floor(perOp.length / 2)],
    minNs: perOp[0],
  };
  console.log(
    `${name.padEnd(28)} median ${(results[name].medianNs / 1000).toFixed(3).padStart(10)} us/op  min ${(results[name].minNs / 1000).toFixed(3).padStart(10)} us/op`,
  );
}
if (sink === undefined) throw new Error("Benchmark produced no value");
if (args.output)
  await writeFile(
    args.output,
    JSON.stringify(
      {
        host: hostname(),
        node: process.version,
        samples,
        minSampleMs,
        results,
      },
      null,
      2,
    ),
  );
