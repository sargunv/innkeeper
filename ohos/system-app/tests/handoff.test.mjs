// Host tests for Innkeeper User's ets/desktop/Handoff.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const handoff = await load("Handoff", "desktop");

test("the desktop opens external links, and nothing else", () => {
  for (const uri of [
    "https://example.org/a",
    "HTTP://example.org",
    "mailto:ada@example.org",
    "geo:0,0",
    "webcals://x",
  ]) {
    assert.ok(handoff.isLink(uri), uri);
  }
  for (const uri of [
    "file:///etc/passwd",
    "innkeeper-probe://x",
    "javascript:alert(1)",
    "https",
    ":https",
    "",
  ]) {
    assert.ok(!handoff.isLink(uri), uri);
  }
});

test("a link handoff opens one URI without files, asking nothing", () => {
  const v = new DataView(handoff.handoffBody(21));
  assert.equal(v.byteLength, 12);
  assert.equal(v.getUint32(0, true), 0);
  assert.deepEqual(
    [v.getUint8(4), v.getUint8(5), v.getUint8(6), v.getUint8(7)],
    [1, 0, 0, 0],
  );
  assert.equal(v.getUint32(8, true), 21);
});

test("the metadata holds the version, the link and an empty subject", () => {
  const link = new TextEncoder().encode("https://example.org");
  const m = handoff.handoffMetadata(link);
  const v = new DataView(m.buffer);
  assert.equal(m.length, 12 + link.length);
  assert.equal(v.getUint32(0, true), 1);
  assert.equal(v.getUint32(4, true), link.length);
  assert.equal(
    new TextDecoder().decode(m.subarray(8, 8 + link.length)),
    "https://example.org",
  );
  assert.equal(v.getUint32(8 + link.length, true), 0);
});
