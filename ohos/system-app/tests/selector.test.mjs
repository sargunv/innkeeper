// Host tests for Innkeeper User's ets/selector/Defaults.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const { defaultType } = await load("Defaults", "selector");
const VIEW = "ohos.want.action.viewData";

test("a view of a local file remembers its specific MIME type", () => {
  assert.equal(
    defaultType(
      VIEW,
      "image/png",
      "file://dev.sargunv.innkeeper.probes/data/storage/el2/base/files/a.png",
    ),
    "image/png",
  );
  assert.equal(
    defaultType(VIEW, "text/plain", "content://media/1"),
    "text/plain",
  );
  assert.equal(defaultType(VIEW, "text/plain", "/data/a.txt"), "text/plain");
});

test("a view without a MIME type remembers the file's extension", () => {
  assert.equal(
    defaultType(
      VIEW,
      "",
      "file://com.ohos.filemanager/data/Download/Photo.PNG",
    ),
    ".png",
  );
  assert.equal(defaultType(VIEW, "", "file://docs/a.tar.gz?x=1#y"), ".gz");
});

test("a view without a type that the bundle manager can look up remembers nothing", () => {
  assert.equal(defaultType(VIEW, "image/*", "file://docs/a.png"), "");
  assert.equal(defaultType(VIEW, "*/*", "file://docs/a.png"), "");
  assert.equal(defaultType(VIEW, "", "file://docs/README"), "");
  assert.equal(defaultType(VIEW, "", "file://docs/.profile"), "");
  assert.equal(defaultType(VIEW, "", "file://docs/a."), "");
  assert.equal(defaultType(VIEW, "", "file://docs.d/README"), "");
  assert.equal(defaultType(VIEW, "", ""), "");
});

test("links and other actions remember nothing", () => {
  assert.equal(defaultType(VIEW, "", "https://example.org/a.png"), "");
  assert.equal(defaultType(VIEW, "image/png", "https://example.org/a.png"), "");
  assert.equal(
    defaultType("ohos.want.action.sendData", "image/png", "file://docs/a.png"),
    "",
  );
});
