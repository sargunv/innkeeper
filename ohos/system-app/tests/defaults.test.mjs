// Host tests for ets/defaults/Defaults.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const { defaultsCommand, defaultsResponse } = await load(
  "Defaults",
  "defaults",
);

const command = (action) =>
  JSON.stringify({ package: "a.b", action, permission: "", enabled: false });

test("info lists an app's defaults, and clear-defaults resets them first", () => {
  assert.deepEqual(defaultsCommand(command("info")), {
    bundle: "a.b",
    clear: false,
  });
  assert.deepEqual(defaultsCommand(command("clear-defaults")), {
    bundle: "a.b",
    clear: true,
  });
});

test("other commands are not defaults commands", () => {
  for (const json of [
    command("background"),
    command("clear-data"),
    '{"action":"info","package":7}',
    '{"action":"launch","package":"a.b","grants":["00000000000000ab"]}',
    "not json",
    "null",
  ]) {
    assert.equal(defaultsCommand(json), undefined, json);
  }
});

test("the response carries Innkeeper User's types or its error", () => {
  const answer = (error, reply) => ({ request: 1, error, reply });
  assert.deepEqual(JSON.parse(defaultsResponse(answer("", '["image/png"]'))), {
    error: "",
    info: null,
    defaults: ["image/png"],
  });
  assert.deepEqual(JSON.parse(defaultsResponse(answer("", "[]"))), {
    error: "",
    info: null,
    defaults: [],
  });
  assert.deepEqual(JSON.parse(defaultsResponse(answer("denied", ""))), {
    error: "denied",
    info: null,
    defaults: null,
  });
  for (const reply of ["", "{}", "[1]", "not json"]) {
    const r = JSON.parse(defaultsResponse(answer("", reply)));
    assert.equal(r.defaults, null, reply);
    assert.notEqual(r.error, "", reply);
  }
});
