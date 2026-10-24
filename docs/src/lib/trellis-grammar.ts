// TextMate grammar for Trellis IDL, used to highlight ```trellis code blocks.
// Keywords follow crates/idl/src/parser.rs; tokens follow crates/idl/src/lexer.rs.

const declarations = [
  "import",
  "from",
  "as",
  "package",
  "model",
  "enum",
  "type",
  "error",
  "api",
  "service",
  "agent",
  "app",
  "device",
];

const members = [
  "public",
  "rpc",
  "event",
  "events",
  "live",
  "operation",
  "job",
  "kv",
  "store",
  "state",
  "upload",
  "download",
  "capabilities",
  "capability",
  "group",
  "implements",
  "use",
  "allows",
  "accepts",
  "subscribe",
  "publish",
  "input",
  "output",
  "payload",
  "result",
  "progress",
  "signals",
  "stream",
  "errors",
  "params",
  "pagination",
  "required",
  "optional",
  "dependency",
  "update",
  "replay",
  "history",
  "path",
  "schema",
  "policy",
  "retry",
  "attempts",
  "backoff",
  "deadline",
  "ttl",
  "concurrency",
  "key_concurrency",
  "desired_max_total",
  "desired_max_object",
  "desired_max_value",
];

const properties = ["title", "description", "grants", "version", "digest"];

const types = [
  "string",
  "bool",
  "int32",
  "uint32",
  "int64",
  "uint64",
  "number",
  "bytes",
  "timestamp",
  "ulid",
  "list",
  "map",
  "CursorPage",
];

const words = (list: string[]) => `\\b(?:${list.join("|")})\\b`;

export const trellisGrammar = {
  name: "trellis",
  scopeName: "source.trellis",
  displayName: "Trellis IDL",
  aliases: ["trellis-idl"],
  patterns: [
    { include: "#comments" },
    { include: "#strings" },
    {
      // "api orders@v1", "model CreateOrderRequest", "service OrdersService"
      match: `\\b(${declarations.join("|")})\\s+([A-Za-z_][A-Za-z0-9_.]*)`,
      captures: {
        1: { name: "keyword.declaration.trellis" },
        2: { name: "entity.name.type.trellis" },
      },
    },
    { match: words(declarations), name: "keyword.declaration.trellis" },
    {
      // "rpc Create", "event Changed", "capability create"
      match: `\\b(${members.join("|")})\\s+([A-Za-z_][A-Za-z0-9_.]*)(?=\\s*[{;])`,
      captures: {
        1: { name: "keyword.other.trellis" },
        2: { name: "entity.name.function.trellis" },
      },
    },
    {
      // "implements orders", "use orders": the API a participant refers to
      match: "\\b(implements|use)\\s+([A-Za-z_][A-Za-z0-9_.]*)",
      captures: {
        1: { name: "keyword.other.trellis" },
        2: { name: "entity.name.type.trellis" },
      },
    },
    { match: words(members), name: "keyword.other.trellis" },
    { match: words(properties), name: "variable.parameter.property.trellis" },
    { match: words(types), name: "support.type.builtin.trellis" },
    { match: "\\b(?:true|false|null)\\b", name: "constant.language.trellis" },
    { match: "@v[0-9]+\\b", name: "constant.other.version.trellis" },
    {
      match: "-?\\b(?:0|[1-9][0-9]*)(?:\\.[0-9]+)?(?:[eE][+-]?[0-9]+)?\\b",
      name: "constant.numeric.trellis",
    },
    { match: "\\b[A-Z][A-Za-z0-9_]*\\b", name: "entity.name.type.trellis" },
    { match: "[{}()\\[\\]<>]", name: "punctuation.section.trellis" },
    { match: "[:;=,?|.]", name: "punctuation.separator.trellis" },
  ],
  repository: {
    comments: {
      patterns: [
        { match: "//.*$", name: "comment.line.double-slash.trellis" },
        { begin: "/\\*", end: "\\*/", name: "comment.block.trellis" },
      ],
    },
    strings: {
      begin: '"',
      end: '"',
      name: "string.quoted.double.trellis",
      patterns: [{ match: "\\\\.", name: "constant.character.escape.trellis" }],
    },
  },
};
