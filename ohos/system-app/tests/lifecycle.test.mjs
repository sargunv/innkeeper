// Host tests for ets/lifecycle/Report.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const report = await load("Report", "lifecycle");
const background = await load("Background", "lifecycle");

test("only user apps' running continuous tasks count", () => {
  const users = new Set(["org.example.player", "org.example.sync"]);
  const tasks = [
    { bundle: "org.example.player", suspended: false },
    { bundle: "org.example.sync", suspended: true },
    { bundle: "com.ohos.system", suspended: false },
  ];
  assert.equal(report.services(tasks, users), 1);
  assert.equal(report.services([], users), 0);
});

test("the body has no windows or wakeup, and it gives the blob's length", () => {
  const v = new DataView(report.body(3, 42));
  assert.equal(v.byteLength, 20);
  assert.equal(v.getUint32(0, true), 0);
  assert.equal(v.getUint32(4, true), 3);
  assert.equal(v.getBigUint64(8, true), 0n);
  assert.equal(v.getUint32(16, true), 42);
});

test("the blob lists background apps as bundle and label lines", () => {
  assert.equal(report.backgroundText([]), "");
  assert.equal(
    report.backgroundText([
      { bundle: "org.example.player", label: "Player" },
      { bundle: "org.example.sync", label: "Sync\tNow\n" },
    ]),
    "org.example.player\tPlayer\norg.example.sync\tSync Now \n",
  );
  const many = Array.from({ length: 40 }, (_, i) => ({
    bundle: `a.b${i}`,
    label: "x",
  }));
  assert.equal(
    report.backgroundText(many).split("\n").length - 1,
    report.MAX_BACKGROUND,
  );
});

test("only the background app command is read", () => {
  const c = (o) => background.backgroundCommand(JSON.stringify(o));
  const command = {
    package: "org.example.player",
    action: "background",
    permission: "",
    enabled: true,
  };
  assert.deepEqual(c(command), { bundle: "org.example.player", enabled: true });
  assert.deepEqual(c({ ...command, enabled: false }), {
    bundle: "org.example.player",
    enabled: false,
  });
  assert.equal(c({ ...command, action: "force-stop" }), undefined);
  assert.equal(c({ action: "windows" }), undefined);
  assert.equal(c({ ...command, enabled: "yes" }), undefined);
  assert.equal(background.backgroundCommand("not json"), undefined);
  assert.equal(background.backgroundCommand("null"), undefined);
});

test("allowing and forbidding keeps the set sorted and unique", () => {
  assert.deepEqual(background.withBundle(["b.c"], "a.b", true), ["a.b", "b.c"]);
  assert.deepEqual(background.withBundle(["a.b", "b.c"], "a.b", true), [
    "a.b",
    "b.c",
  ]);
  assert.deepEqual(background.withBundle(["a.b", "b.c"], "a.b", false), [
    "b.c",
  ]);
  assert.deepEqual(background.withBundle([], "a.b", false), []);
});

test("responses carry an error and no info", () => {
  assert.deepEqual(JSON.parse(background.response("")), {
    error: "",
    info: null,
  });
  assert.deepEqual(JSON.parse(background.response("unknown package a.b")), {
    error: "unknown package a.b",
    info: null,
  });
});
