// Host tests for ets/open/Launch.ets, ets/service/Answer.ets and Innkeeper
// User's ets/files/Docs.ets and ets/files/Targets.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const launch = await load("Launch", "open");
const answer = await load("Answer", "service");
const docs = await load("Docs", "files");
const targets = await load("Targets", "files");

const command = (fields) =>
  JSON.stringify({
    action: "launch",
    package: "org.example.viewer",
    component: "",
    uri: "",
    grants: ["00000000000000ab"],
    token: "",
    wait: false,
    new_window: false,
    width: 0,
    height: 0,
    ...fields,
  });

test("a launch with grants opens files in its bundle or in OpenHarmony's choice", () => {
  assert.deepEqual(launch.fileLaunch(command({})), {
    bundle: "org.example.viewer",
    grants: ["00000000000000ab"],
  });
  assert.deepEqual(
    launch.fileLaunch(
      command({
        package: launch.CHOOSER,
        grants: ["0000000000000001", "0000000000000002"],
      }),
    ),
    { bundle: "", grants: ["0000000000000001", "0000000000000002"] },
  );
});

test("other commands and malformed grants are not file launches", () => {
  for (const json of [
    command({ grants: [] }),
    command({ action: "windows" }),
    command({ grants: ["ab"] }),
    command({ grants: ["00000000000000AB"] }),
    command({ grants: [7] }),
    command({ package: 1 }),
    '{"package":"a.b","action":"background","enabled":true}',
    "not json",
    "null",
  ]) {
    assert.equal(launch.fileLaunch(json), undefined, json);
  }
});

test("the window response carries Innkeeper User's reply or its error", () => {
  const ok = {
    request: 1,
    error: "",
    reply: "opened 1 file in org.example.viewer",
  };
  assert.deepEqual(JSON.parse(launch.launched(ok)), {
    error: "",
    reply: "opened 1 file in org.example.viewer",
  });
  assert.deepEqual(
    JSON.parse(launch.launched({ request: 1, error: "no", reply: "x" })),
    { error: "no", reply: null },
  );
});

test("Innkeeper User's answer comes from the want's parameters", () => {
  const p = {
    "innkeeper.answer.request": 7,
    "innkeeper.answer.error": "",
    "innkeeper.answer.reply": "opened 1 file in a.b",
  };
  assert.deepEqual(
    { ...answer.userAnswer(p) },
    { request: 7, error: "", reply: "opened 1 file in a.b" },
  );
  assert.equal(answer.userAnswer(undefined), undefined);
  assert.equal(answer.userAnswer({}), undefined);
  assert.equal(
    answer.userAnswer({ ...p, "innkeeper.answer.request": "7" }),
    undefined,
  );
  assert.equal(
    answer.userAnswer({ ...p, "innkeeper.answer.error": undefined }),
    undefined,
  );
});

test("OPEN and STAT bodies carry the grant, the read mode and an empty path", () => {
  const open = new DataView(docs.openBody("0102030405060708"));
  assert.equal(open.byteLength, 11);
  assert.equal(open.getBigUint64(0, true), 0x0102030405060708n);
  assert.equal(open.getUint8(8), 1);
  assert.equal(open.getUint16(9, true), 0);
  const stat = new DataView(docs.statBody("ffffffffffffffff"));
  assert.equal(stat.byteLength, 10);
  assert.equal(stat.getBigUint64(0, true), 0xffffffffffffffffn);
  assert.equal(stat.getUint16(8, true), 0);
});

// A STAT reply: kind, size, mtime, writable, then the name.
function meta(kind, name, extra = 0) {
  const n = new TextEncoder().encode(name);
  const b = new ArrayBuffer(20 + n.length + extra);
  const v = new DataView(b);
  v.setUint8(0, kind);
  v.setBigUint64(1, 5n, true);
  v.setUint16(18, n.length, true);
  new Uint8Array(b, 20).set(n);
  return b;
}

test("a STAT reply gives a file's name, and nothing for folders or bad names", () => {
  const decode = (b) => new TextDecoder().decode(b);
  assert.equal(docs.statName(meta(0, "notes.txt"), decode), "notes.txt");
  assert.equal(docs.statName(meta(1, "folder"), decode), undefined);
  assert.equal(docs.statName(meta(0, "a.txt", 1), decode), undefined);
  assert.equal(docs.statName(meta(0, "a.txt").slice(0, 22), decode), undefined);
  assert.equal(docs.statName(new ArrayBuffer(4), decode), undefined);
  for (const bad of ["", ".", "..", "a/b"]) {
    assert.equal(docs.statName(meta(0, bad), decode), undefined, bad);
  }
  assert.equal(
    docs.statName(meta(0, "x"), () => undefined),
    undefined,
  );
});

test("extensions are lower case with their dot", () => {
  assert.equal(docs.extension("Photo.PNG"), ".png");
  assert.equal(docs.extension("archive.tar.gz"), ".gz");
  assert.equal(docs.extension("README"), "");
  assert.equal(docs.extension(".profile"), "");
});

test("the want views the first file and streams every file with the read grant", () => {
  const one = docs.openWant(
    "org.example.viewer",
    ["file://a/1.txt"],
    "text/plain",
    1,
  );
  assert.equal(one.bundleName, "org.example.viewer");
  assert.equal(one.action, "ohos.want.action.viewData");
  assert.equal(one.uri, "file://a/1.txt");
  assert.equal(one.type, "text/plain");
  assert.equal(one.flags, 1);
  assert.equal(one.parameters, undefined);
  const two = docs.openWant("", ["file://a/1.png", "file://a/2.png"], "", 1);
  assert.equal(two.bundleName, undefined);
  assert.equal(two.type, undefined);
  assert.deepEqual(two.parameters, {
    "ability.params.stream": ["file://a/1.png", "file://a/2.png"],
  });
});

test("a target's want names its ability and grants every file", () => {
  const w = docs.targetWant(
    { bundleName: "a.b", moduleName: "entry", abilityName: "View" },
    ["file://a/1.png", "file://a/2.txt"],
    ["image/png", "text/plain"],
    1,
  );
  assert.equal(w.bundleName, "a.b");
  assert.equal(w.moduleName, "entry");
  assert.equal(w.abilityName, "View");
  assert.equal(w.uri, "file://a/1.png");
  assert.equal(w.type, "image/png");
  assert.equal(w.flags, 1);
  assert.deepEqual(w.parameters, {
    "ability.params.stream": ["file://a/1.png", "file://a/2.txt"],
  });
});

const ability = (bundleName, name = "Main", exported = true) => ({
  bundleName,
  moduleName: "entry",
  name,
  exported,
});

test("targets take every type of the batch, once each", () => {
  const photos = ability("com.ohos.photos");
  const probe = ability("org.example.probe");
  const reader = ability("org.example.reader");
  assert.deepEqual(
    targets.targets([[photos, probe, photos]], "own").map((t) => t.bundleName),
    ["com.ohos.photos", "org.example.probe"],
  );
  assert.deepEqual(
    targets.targets(
      [
        [photos, probe],
        [reader, probe],
      ],
      "own",
    ),
    [
      {
        bundleName: "org.example.probe",
        moduleName: "entry",
        abilityName: "Main",
      },
    ],
  );
  assert.deepEqual(targets.targets([[photos], []], "own"), []);
  assert.deepEqual(targets.targets([], "own"), []);
});

test("targets leave out private abilities and the caller's own", () => {
  assert.deepEqual(
    targets
      .targets(
        [
          [
            ability("a.b", "Hidden", false),
            ability("own"),
            ability("a.b", "View"),
          ],
        ],
        "own",
      )
      .map((t) => t.abilityName),
    ["View"],
  );
});

test("a batch without a target names its type", () => {
  assert.equal(
    targets.noTarget("org.example.probe", ["application/pdf"]),
    "org.example.probe doesn't open application/pdf files",
  );
  assert.equal(
    targets.noTarget("", ["application/pdf", "application/pdf"]),
    "No OpenHarmony app opens application/pdf files",
  );
  assert.equal(
    targets.noTarget("", ["text/plain", "image/png"]),
    "No OpenHarmony app opens these files",
  );
  assert.equal(
    targets.noTarget("", [""]),
    "No OpenHarmony app opens these files",
  );
});

test("OpenHarmony's error codes become plain messages", () => {
  const e = (code, bundle = "org.example.probe") =>
    targets.openError(code, "raw", bundle, ["image/png"]);
  assert.equal(e(undefined), "raw");
  assert.equal(e(17700001), "org.example.probe isn't installed");
  assert.equal(e(17700001, ""), "No OpenHarmony app opens image/png files");
  for (const code of [17700003, 16000001, 16000019]) {
    assert.equal(e(code), "org.example.probe doesn't open image/png files");
  }
  assert.equal(e(17700026), "org.example.probe is disabled");
  assert.equal(e(17700029, ""), "The app is disabled");
  assert.equal(
    e(16000004),
    "org.example.probe doesn't let other apps open files",
  );
  assert.equal(
    e(16000050),
    "OpenHarmony couldn't open the files (16000050 raw)",
  );
});

test("the reply names the app or the chooser", () => {
  assert.equal(targets.opened(1, "a.b"), "opened 1 file in a.b");
  assert.equal(targets.opened(2, ""), "showing the app chooser for 2 files");
});

test("distinct types keep their order", () => {
  assert.deepEqual(targets.distinctTypes(["a", "b", "a", ""]), ["a", "b", ""]);
});
