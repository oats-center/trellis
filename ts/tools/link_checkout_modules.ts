// Makes a checkout without its own npm install resolve its own packages.
//
// A git worktree has no node_modules, so Node and TypeScript would walk up to
// the main checkout's install and load the main checkout's
// `@oatscenter/*` packages instead of this checkout's `ts/packages/*`. This
// script gives such a checkout a node_modules of links: each workspace package
// points at this checkout's own directory, and every third-party module points
// at the main checkout's install. A checkout npm installed itself (an ordinary
// clone, the main checkout, CI) already has the right links, so the script
// leaves it untouched. Test runners and checks run it before they start;
// running it again is cheap and repairs links that went stale.
import { execFileSync } from "node:child_process";
import {
  existsSync,
  globSync,
  lstatSync,
  mkdirSync,
  readdirSync,
  readFileSync,
  readlinkSync,
  rmSync,
  symlinkSync,
} from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

const root = resolve(import.meta.dirname, "../..");
const modules = join(root, "node_modules");
// npm writes this file into every install it owns.
const npmOwned = (directory: string) =>
  existsSync(join(directory, ".package-lock.json"));

if (!npmOwned(modules)) {
  let main: string | undefined;
  try {
    main = dirname(
      execFileSync(
        "git",
        ["rev-parse", "--path-format=absolute", "--git-common-dir"],
        { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] },
      ).trim(),
    );
  } catch {
    // Not a git checkout: nothing to share an install with.
  }
  // A checkout that is not a worktree has no other install to share.
  const shared =
    main === undefined || main === root
      ? undefined
      : join(main, "node_modules");
  if (shared !== undefined && !npmOwned(shared)) {
    console.error(
      `no npm install to share: run \`npm ci\` in the main checkout ${main}`,
    );
  } else if (shared !== undefined) {
    // Published path under node_modules -> link target.
    const wanted = new Map<string, string>();
    // npm installs third-party modules as directories; its symlinks are the
    // main checkout's own workspace packages, which this checkout replaces.
    const addShared = (name: string) => {
      if (!lstatSync(join(shared, name)).isSymbolicLink())
        wanted.set(name, join(shared, name));
    };
    for (const entry of readdirSync(shared)) {
      if (entry.startsWith("@")) {
        for (const member of readdirSync(join(shared, entry)))
          addShared(`${entry}/${member}`);
      } else if (!entry.startsWith(".") || entry === ".bin") {
        addShared(entry);
      }
    }
    const manifest = JSON.parse(
      readFileSync(join(root, "package.json"), "utf8"),
    ) as { workspaces: string[] };
    for (const directory of globSync(manifest.workspaces, { cwd: root })) {
      const packageJson = join(root, directory, "package.json");
      if (!existsSync(packageJson)) continue;
      const { name } = JSON.parse(readFileSync(packageJson, "utf8")) as {
        name: string;
      };
      wanted.set(
        name,
        relative(dirname(join(modules, name)), join(root, directory)),
      );
    }

    // Drop links this script made for modules that no longer exist.
    if (existsSync(modules)) {
      for (const entry of readdirSync(modules)) {
        const names = entry.startsWith("@")
          ? readdirSync(join(modules, entry)).map(
              (member) => `${entry}/${member}`,
            )
          : [entry];
        for (const name of names) {
          if (
            lstatSync(join(modules, name)).isSymbolicLink() &&
            !wanted.has(name)
          )
            rmSync(join(modules, name));
        }
      }
    }

    const current = (path: string) => {
      try {
        return readlinkSync(path);
      } catch {
        return undefined;
      }
    };
    for (const [name, target] of wanted) {
      const path = join(modules, name);
      if (current(path) === target) continue;
      // Only ever replaces a link; a real directory makes this throw.
      rmSync(path, { force: true });
      mkdirSync(dirname(path), { recursive: true });
      try {
        symlinkSync(target, path, "dir");
      } catch (error) {
        // A concurrent run may have made the same link first.
        if (current(path) !== target) throw error;
      }
    }
  }
}
