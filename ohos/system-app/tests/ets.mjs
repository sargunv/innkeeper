// Loads a module of ets/defaults, ets/ime, ets/lifecycle, ets/media,
// ets/notify, ets/open, ets/service, ets/settings or Innkeeper User's
// ets/clipboard, ets/desktop, ets/files and ets/selector under Node. Its ArkTS is plain TypeScript, so Node runs a copy as .ts with
// its types stripped. The copies name their relative imports with the .ts
// extension, which Node requires.
import { readdirSync, readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
// node --test runs the test files in parallel processes, so each one copies
// the modules into a directory of its own.
const out = join(
  here,
  "../../../out/build/ohos-system-app-tests",
  String(process.pid),
);
const user = ["clipboard", "desktop", "files", "selector"];
for (const dir of [
  "defaults",
  "ime",
  "lifecycle",
  "media",
  "notify",
  "open",
  "service",
  "settings",
  ...user,
]) {
  const src = join(here, user.includes(dir) ? "../user/ets" : "../ets", dir);
  mkdirSync(join(out, dir), { recursive: true });
  for (const f of readdirSync(src).filter((f) => f.endsWith(".ets"))) {
    const code = readFileSync(join(src, f), "utf8").replace(
      /from '(\.\.?\/[^']+)'/g,
      "from '$1.ts'",
    );
    writeFileSync(join(out, dir, f.replace(/\.ets$/, ".ts")), code);
  }
}

export function load(name, dir = "ime") {
  return import(join(out, dir, `${name}.ts`));
}
