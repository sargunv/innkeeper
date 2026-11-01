// Host tests for ets/lifecycle/Report.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const report = await load("Report", "lifecycle");

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

test("the body has no windows or wakeup and no blob", () => {
  const v = new DataView(report.body(3));
  assert.equal(v.byteLength, 20);
  assert.equal(v.getUint32(0, true), 0);
  assert.equal(v.getUint32(4, true), 3);
  assert.equal(v.getBigUint64(8, true), 0n);
  assert.equal(v.getUint32(16, true), 0);
});
