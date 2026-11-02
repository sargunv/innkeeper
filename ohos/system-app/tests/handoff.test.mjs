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

test("a want's file URI goes to the desktop as a file", () => {
  assert.ok(handoff.isFile("file://com.example/data/storage/a.png"));
  assert.ok(handoff.isFile("FILE:///a"));
  assert.ok(!handoff.isFile("https://example.org/a.png"));
  assert.ok(!handoff.isFile("files://x"));
  assert.ok(!handoff.isLink("file://com.example/a.png"));
});

test("a file's name is its URI's last segment, decoded", () => {
  const name = handoff.fileName;
  assert.equal(
    name("file://com.example/data/storage/el2/base/files/a.png"),
    "a.png",
  );
  assert.equal(
    name("file://docs/storage/Users/currentUser/My%20Notes.txt"),
    "My Notes.txt",
  );
  assert.equal(
    name("file://com.example/a/caf%C3%A9.odt?networkid=1#x"),
    "café.odt",
  );
  // A name that isn't valid percent-encoding stays as it is.
  assert.equal(name("file://com.example/a/100%.txt"), "100%.txt");
  // Separators and control characters that decode into the name don't reach
  // the desktop.
  assert.equal(name("file://com.example/a/x%2Fy%5Cz%0A.txt"), "x_y_z.txt");
  for (const uri of [
    "file://com.example/a/",
    "file://com.example/a/..",
    "file://com.example/%2E",
    "file:",
  ]) {
    assert.equal(name(uri), "Shared file", uri);
  }
});

test("a name is cut before a character that doesn't fit", () => {
  const b = new TextEncoder().encode("aé");
  assert.deepEqual([...handoff.truncateUtf8(b, 3)], [...b]);
  assert.deepEqual([...handoff.truncateUtf8(b, 2)], [0x61]);
  assert.deepEqual([...handoff.truncateUtf8(b, 1)], [0x61]);
});

test("a link handoff opens one URI without files, asking nothing", () => {
  const v = new DataView(handoff.handoffBody(handoff.URI, 0, 21));
  assert.equal(v.byteLength, 12);
  assert.equal(v.getUint32(0, true), 0);
  assert.deepEqual(
    [v.getUint8(4), v.getUint8(5), v.getUint8(6), v.getUint8(7)],
    [1, 0, 0, 0],
  );
  assert.equal(v.getUint32(8, true), 21);
});

test("a file handoff opens one file in the desktop's default app", () => {
  const v = new DataView(handoff.handoffBody(handoff.OPEN, 1, 30));
  assert.deepEqual(
    [v.getUint8(4), v.getUint8(5), v.getUint8(6), v.getUint8(7)],
    [2, 1, 0, 0],
  );
  assert.equal(v.getUint32(8, true), 30);
});

test("the metadata holds the version, the link and an empty subject", () => {
  const link = new TextEncoder().encode("https://example.org");
  const m = handoff.handoffMetadata(link, []);
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

test("a file's metadata has empty text and the file's name, cut to 255 bytes", () => {
  const enc = new TextEncoder();
  const m = handoff.handoffMetadata(new Uint8Array(0), [enc.encode("a.png")]);
  const v = new DataView(m.buffer);
  assert.deepEqual(
    [
      v.getUint32(0, true),
      v.getUint32(4, true),
      v.getUint32(8, true),
      v.getUint32(12, true),
    ],
    [1, 0, 0, 5],
  );
  assert.equal(new TextDecoder().decode(m.subarray(16)), "a.png");

  const long = enc.encode("é".repeat(200));
  const cut = handoff.handoffMetadata(new Uint8Array(0), [long]);
  const name = new DataView(cut.buffer).getUint32(12, true);
  assert.equal(name, 254);
  assert.equal(cut.length, 16 + 254);
});
