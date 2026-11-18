# Codec performance

Measures the shared TypeScript codecs that generated packages compose
(`ts/packages/trellis/generated_support.ts`) in one Node process, with no
runtime or network. Run from the repository root:

```sh
npm run bench:codecs -- --output=/path/to/codecs.json
```

Each case calibrates an iteration count so one sample runs for at least
`--min-sample-ms` (default 50), then reports the median and minimum time per
operation over `--samples` samples (default 15). The host is shared and
unpinned, so compare runs taken back to back and prefer the minimum when the
median is noisy.

Cases:

- `list-10000-narrow`: a list of 10,000 four-field models, with an optional
  nullable field absent, explicitly `null`, or set.
- `list-1000-wide`: a list of 1,000 models with 48 fields (16 optional
  nullable, half absent and a quarter `null`), a nested 16-field model, eight
  unknown top-level fields and one unknown nested field.
- `bounded-4096-ascii` and `bounded-4096-astral`: a `string` bounded to
  1..4096 code points, given 4,096 ASCII characters or 4,096 astral
  characters (8,192 UTF-16 units).

## Results

Node 26.13.1, Linux x86_64, 31 samples, before and after precomputing model
field tables, skipping the unknown-field copy when every own key is known, and
counting bounded strings without building a code-point array. Medians in
microseconds per operation:

| Case                         | Before | After |
| ---------------------------- | -----: | ----: |
| `list-10000-narrow/decode`   |  3,549 | 2,796 |
| `list-10000-narrow/encode`   |  3,110 | 2,688 |
| `list-1000-wide/decode`      | 19,777 | 7,327 |
| `list-1000-wide/encode`      | 19,897 | 7,396 |
| `bounded-4096-ascii/decode`  |   24.7 | 0.003 |
| `bounded-4096-astral/decode` |   43.9 |  19.8 |
| `bounded-4096-astral/encode` |   43.9 |  19.2 |

The ASCII case no longer counts at all: a string has at least half its UTF-16
length and at most its UTF-16 length in code points, so when that whole range
satisfies the bounds the exact count cannot change the outcome. The astral case
is twice the bound in UTF-16 units and is counted exactly.
