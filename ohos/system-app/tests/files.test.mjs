// Host tests for ets/open/Launch.ets and Innkeeper User's ets/files/Docs.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const launch = await load("Launch", "open");
const docs = await load("Docs", "files");

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

test("the window response names the app or the chooser", () => {
  const one = { bundle: "org.example.viewer", grants: ["00000000000000ab"] };
  assert.deepEqual(JSON.parse(launch.launched(one, "")), {
    error: "",
    reply: "opening 1 file in org.example.viewer",
  });
  const two = { bundle: "", grants: ["0000000000000001", "0000000000000002"] };
  assert.equal(
    JSON.parse(launch.launched(two, "")).reply,
    "opening 2 files in the chosen app",
  );
  assert.deepEqual(JSON.parse(launch.launched(one, "no")), {
    error: "no",
    reply: null,
  });
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
