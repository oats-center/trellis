type Digit = "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9";
type CharacterKind = "upper" | "lower" | "digit" | "separator";

type KindOf<TChar extends string> = TChar extends Digit ? "digit"
  : TChar extends Lowercase<TChar>
    ? TChar extends Uppercase<TChar> ? "separator" : "lower"
  : "upper";

type FirstKind<TValue extends string> = TValue extends
  `${infer TChar}${infer _Rest}` ? KindOf<TChar>
  : "separator";

type AppendSeparator<TValue extends string> = TValue extends "" ? ""
  : TValue extends `${string}_` ? TValue
  : `${TValue}_`;

type SnakeSurfaceName<
  TValue extends string,
  TPrevious extends CharacterKind = "separator",
  TOutput extends string = "",
> = TValue extends `${infer TChar}${infer TRest}`
  ? KindOf<TChar> extends infer TKind extends CharacterKind
    ? TKind extends "separator"
      ? SnakeSurfaceName<TRest, "separator", AppendSeparator<TOutput>>
    : TKind extends "upper" ? SnakeSurfaceName<
        TRest,
        "upper",
        `${TOutput}${TOutput extends "" ? ""
          : TPrevious extends "separator" ? ""
          : TPrevious extends "lower" | "digit" ? "_"
          : FirstKind<TRest> extends "lower" ? "_"
          : ""}${Lowercase<TChar>}`
      >
    : SnakeSurfaceName<TRest, TKind, `${TOutput}${TChar}`>
  : never
  : TOutput extends `${infer TTrimmed}_` ? TTrimmed
  : TOutput;

type PascalFromSnake<TValue extends string> = TValue extends
  `${infer THead}_${infer TTail}`
  ? `${Capitalize<THead>}${PascalFromSnake<TTail>}`
  : Capitalize<TValue>;

/** The deterministic PascalCase identifier for a canonical surface name. */
export type PascalActionName<TName extends string> = PascalFromSnake<
  SnakeSurfaceName<TName>
>;

/** The deterministic lowerCamelCase identifier for a canonical surface name. */
export type ConnectedActionName<TName extends string> = Uncapitalize<
  PascalActionName<TName>
>;

function snakeSurfaceName(value: string): string {
  const chars = [...value];
  let output = "";
  let previousWasSeparator = false;

  for (let index = 0; index < chars.length; index++) {
    const char = chars[index]!;
    if (/^[A-Za-z0-9]$/.test(char)) {
      const previous = chars[index - 1];
      const next = chars[index + 1];
      const startsNewWord = /^[A-Z]$/.test(char) && output !== "" &&
        !previousWasSeparator &&
        (previous !== undefined && /^[a-z0-9]$/.test(previous) ||
          next !== undefined && /^[a-z]$/.test(next));
      if (startsNewWord) output += "_";
      output += char.toLowerCase();
      previousWasSeparator = false;
    } else if (output !== "" && !previousWasSeparator) {
      output += "_";
      previousWasSeparator = true;
    }
  }

  return output.replace(/_$/, "");
}

/** Converts a canonical surface name to its acronym-aware PascalCase identifier. */
export function pascalSurfaceName(value: string): string {
  return snakeSurfaceName(value).split("_").filter(Boolean).map((word) =>
    word[0]!.toUpperCase() + word.slice(1)
  ).join("");
}

/** Converts a canonical surface name to its acronym-aware lowerCamelCase identifier. */
export function lowerCamelSurfaceName(value: string): string {
  const pascal = pascalSurfaceName(value);
  return pascal[0] ? pascal[0].toLowerCase() + pascal.slice(1) : "";
}
