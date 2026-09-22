import type { AppTypes } from "$app/types";
import { routeTitles } from "./control-panel.ts";

type AppPathname = ReturnType<AppTypes["Pathname"]>;
type ConsolePathname<T = AppPathname> = T extends `/console/(app)${infer Path}`
  ? Path extends "" ? "/" : Path
  : T extends `/console${infer Path}` ? Path extends "" ? "/" : Path
  : never;

function checkRouteTitles<T extends Partial<Record<ConsolePathname, string>>>(
  titles: T & Record<Exclude<keyof T, ConsolePathname>, never>,
): void {
  void titles;
}

checkRouteTitles(routeTitles);
