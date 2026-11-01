// Compiles an app's ArkTS into ets/modules.abc with the SDK's ets-loader,
// which hvigor otherwise runs. hap.sh sets the environment that ets-loader's
// main.js reads (aceModuleJsonPath, aceModuleRoot, aceModuleBuild and the
// rest).
//
// usage: node arkts.js <ets-loader dir> <rollup dir>
//
// ets-loader's plugins are rollup plugins, and hvigor runs them on its own
// rollup fork. Upstream rollup 3 runs them once the plugin context has what
// the fork adds:
// - `share`, which holds the project config and the loggers;
// - `getModuleInfo(id).importedIdMaps`, each import's resolved id;
// - `getModuleInfo(id).getNodeByType(...)`, the module's AST nodes by type;
// - `async(fn)` and `signal()`, which start and finish work that the build
//   waits for, such as es2abc.
"use strict";
const path = require("path");

const [loader, rollupDir] = process.argv.slice(2);
process.env.compileTool = "rollup";
process.env.watchMode = "false";
const { rollup } = require(path.join(rollupDir, "dist/rollup.js"));
const plugin = require(path.join(loader, "compile_plugin.js"));
const main = require(path.join(loader, "main.js"));

const { entryObj } = plugin.initConfig();
const config = main.projectConfig;
config.buildMode = process.env.buildMode;
config.compatibleSdkVersion = process.env.apiVersion;
// The checker treats files under the loader's SDK as SDK declarations.
config.etsLoaderPath = loader;

let failed = false;
const say =
  (level) =>
  (...msg) => {
    if (level === "error") failed = true;
    console.error(`arkts: ${level}:`, ...msg);
  };
const logger = {
  debug: () => {},
  info: () => {},
  warn: say("warning"),
  error: say("error"),
  printError: say("error"),
  printErrorAndExit: (...msg) => {
    say("error")(...msg);
    process.exit(1);
  },
};
const share = {
  projectConfig: config,
  getLogger: () => logger,
  getHvigorConsoleLogger: () => logger,
  throwArkTsCompilerError: (msg) => {
    throw new Error(String(msg));
  },
};

// importer -> (import specifier -> resolved id)
const imports = new Map();
let pending = 0;
let idle = () => {};

function nodesByType(node, types, out) {
  if (!node || typeof node.type !== "string") return;
  if (types.includes(node.type)) {
    if (!out.has(node.type)) out.set(node.type, []);
    out.get(node.type).push(node);
  }
  for (const v of Object.values(node)) {
    if (Array.isArray(v)) v.forEach((c) => nodesByType(c, types, out));
    else if (v && typeof v === "object") nodesByType(v, types, out);
  }
}

// Adds the fork's members to a plugin context, once per context.
function extend(ctx) {
  if (ctx.share) return;
  ctx.share = share;
  ctx.async = (fn) => {
    pending++;
    return fn();
  };
  ctx.signal = () => {
    if (--pending === 0) idle();
  };
  const moduleInfo = ctx.getModuleInfo.bind(ctx);
  ctx.getModuleInfo = (id) => {
    const info = moduleInfo(id);
    if (!info) return info;
    return Object.assign(Object.create(info), {
      importedIdMaps: Object.fromEntries(imports.get(id) || []),
      getNodeByType: (...types) => {
        const out = new Map();
        nodesByType(info.ast, types, out);
        return out;
      },
    });
  };
}

function withContext(p) {
  for (const [k, v] of Object.entries(p)) {
    if (typeof v === "function") {
      p[k] = function (...a) {
        extend(this);
        return v.apply(this, a);
      };
    } else if (v && typeof v.handler === "function") {
      const h = v.handler;
      v.handler = function (...a) {
        extend(this);
        return h.apply(this, a);
      };
    }
  }
  return p;
}

// hvigor's rollup resolves a relative import without an extension to an
// .ets or .ts file.
const fs = require("fs");
const resolveArkts = {
  name: "resolve-arkts",
  resolveId(source, importer) {
    if (!importer || !source.startsWith(".")) return null;
    const base = path.resolve(path.dirname(importer), source);
    for (const ext of [".ets", ".ts"]) {
      if (fs.existsSync(base + ext)) return base + ext;
    }
    return null;
  },
};

const recordImports = {
  name: "record-imports",
  async resolveId(source, importer, opts) {
    if (!importer) return null;
    const r = await this.resolve(source, importer, { ...opts, skipSelf: true });
    if (!imports.has(importer)) imports.set(importer, new Map());
    imports.get(importer).set(source, r ? r.id : source);
    return r;
  },
};

(async () => {
  const plugins = plugin.sdkPlugins(config).filter(Boolean).map(withContext);
  const bundle = await rollup({
    input: entryObj,
    plugins: [resolveArkts, recordImports, ...plugins],
    // System modules such as @ohos.app.ability.UIAbility come from the
    // device, and native modules such as libfoo.so from the HAP's libs, so
    // rollup can't resolve them.
    onwarn: (w) => {
      if (
        w.code !== "UNRESOLVED_IMPORT" ||
        !/^(@(ohos|kit|system|arkts)\.|lib.+\.so$)/.test(w.exporter)
      ) {
        logger.warn(w.message);
      }
    },
  });
  // genAbc writes modules.abc. The JavaScript bundle isn't needed.
  await bundle.generate({ format: "es" });
  await bundle.close();
  if (pending > 0) await new Promise((r) => (idle = r));
  process.exit(failed ? 1 : 0);
})().catch((e) => {
  console.error("arkts:", e);
  process.exit(1);
});
