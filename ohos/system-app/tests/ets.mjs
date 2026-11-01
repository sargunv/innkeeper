// Loads a module of ets/ime under Node. Its ArkTS is plain TypeScript, so Node
// runs a copy as .ts with its types stripped. The copies name their relative
// imports with the .ts extension, which Node requires.
import { readdirSync, readFileSync, mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const src = join(here, "../ets/ime");
const out = join(here, "../../../out/build/ohos-system-app-tests");
mkdirSync(out, { recursive: true });
for (const f of readdirSync(src).filter((f) => f.endsWith(".ets"))) {
  const code = readFileSync(join(src, f), "utf8").replace(
    /from '(\.\/[^']+)'/g,
    "from '$1.ts'",
  );
  writeFileSync(join(out, f.replace(/\.ets$/, ".ts")), code);
}

export function load(name) {
  return import(join(out, `${name}.ts`));
}
