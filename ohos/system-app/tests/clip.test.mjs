// Host tests for ets/clipboard/Clip.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const clip = await load("Clip", "clipboard");

const encoder = new TextEncoder();
const decoder = new TextDecoder("utf-8", { fatal: true });
const utf8 = {
  encode: (s) => encoder.encode(s),
  decode: (b) => {
    try {
      return decoder.decode(b);
    } catch {
      return undefined;
    }
  },
};

function file(name, mime, bytes) {
  return Object.assign(new clip.ClipFile(), {
    name,
    mime,
    bytes: Uint8Array.from(bytes),
  });
}

function item(fields) {
  return Object.assign(new clip.Item(), fields);
}

const notImage = () => false;

test("a CLIP header decodes, and a CLIP_SET header is never sensitive", () => {
  const b = new Uint8Array([2, 1, 0, 0, 0x10, 0, 0, 0]).buffer;
  assert.deepEqual(
    { ...clip.decodeHeader(b) },
    {
      kind: 2,
      sensitive: true,
      len: 16,
    },
  );
  assert.equal(clip.decodeHeader(new ArrayBuffer(7)), undefined);
  assert.deepEqual(
    [...new Uint8Array(clip.encodeHeader(clip.FILES, 0x01020304))],
    [3, 0, 0, 0, 4, 3, 2, 1],
  );
});

test("text over the cap is cut at a whole character", () => {
  // é is two bytes, so a cut after three bytes keeps "aé".
  const b = encoder.encode("aéb");
  assert.deepEqual([...clip.truncateUtf8(b, 3)], [...encoder.encode("aé")]);
  assert.deepEqual([...clip.truncateUtf8(b, 2)], [...encoder.encode("a")]);
  assert.equal(clip.truncateUtf8(b, 4), b);
});

test("an image round-trips, and a wrong size is refused", () => {
  const img = Object.assign(new clip.Rgba(), {
    width: 2,
    height: 1,
    pixels: Uint8Array.from([1, 2, 3, 4, 5, 6, 7, 8]),
  });
  const b = clip.encodeImage(img);
  assert.deepEqual([...b.subarray(0, 4)], [2, 0, 1, 0]);
  const back = clip.decodeImage(b);
  assert.equal(back.width, 2);
  assert.equal(back.height, 1);
  assert.deepEqual([...back.pixels], [1, 2, 3, 4, 5, 6, 7, 8]);
  assert.equal(clip.decodeImage(b.subarray(0, 11)), undefined);
  const big = new Uint8Array(4);
  new DataView(big.buffer).setUint16(0, 2049, true);
  new DataView(big.buffer).setUint16(2, 1, true);
  assert.equal(clip.decodeImage(big), undefined);
});

test("pixels lose their row padding, BGRA swaps, and premultiplied alpha straightens", () => {
  // Two rows of one pixel, each padded to eight bytes.
  const src = Uint8Array.from([
    10, 20, 30, 255, 0, 0, 0, 0, 40, 50, 60, 128, 0, 0, 0, 0,
  ]);
  const rgba = clip.toRgba(src, 1, 2, 8, clip.RGBA, false);
  assert.deepEqual([...rgba.pixels], [10, 20, 30, 255, 40, 50, 60, 128]);
  const bgra = clip.toRgba(src, 1, 2, 8, clip.BGRA, false);
  assert.deepEqual([...bgra.pixels], [30, 20, 10, 255, 60, 50, 40, 128]);
  const straight = clip.toRgba(src, 1, 2, 8, clip.RGBA, true);
  assert.deepEqual([...straight.pixels], [10, 20, 30, 255, 80, 100, 120, 128]);
});

test("an image over 2048 pixels on a side scales to fit", () => {
  assert.equal(clip.fitScale(4096, 1024), 0.5);
  assert.equal(clip.fitScale(2048, 2048), 1);
});

test("a file list round-trips in the bridge's format", () => {
  const files = [
    file("notes.txt", "text/plain", [104, 105]),
    file("café.png", "image/png", []),
  ];
  const b = clip.encodeFiles(files, utf8);
  assert.equal(b.length, clip.filesSize(files, utf8));
  const v = new DataView(b.buffer);
  assert.equal(v.getUint32(0, true), 2);
  assert.equal(v.getUint32(4, true), 9);
  const back = clip.decodeFiles(b, utf8);
  assert.deepEqual(
    back.map((f) => [f.name, f.mime, [...f.bytes]]),
    [
      ["notes.txt", "text/plain", [104, 105]],
      ["café.png", "image/png", []],
    ],
  );
});

test("a file list that the bridge wouldn't send is refused", () => {
  const ok = clip.encodeFiles([file("a", "text/plain", [1])], utf8);
  assert.equal(
    clip.decodeFiles(ok.subarray(0, ok.length - 1), utf8),
    undefined,
  );
  const trailing = new Uint8Array(ok.length + 1);
  trailing.set(ok);
  assert.equal(clip.decodeFiles(trailing, utf8), undefined);
  assert.equal(clip.decodeFiles(new Uint8Array(4), utf8), undefined);
  for (const name of ["", ".", "..", "a/b", "a\\b", "a\nb"]) {
    const b = clip.encodeFiles([file(name, "text/plain", [])], utf8);
    assert.equal(clip.decodeFiles(b, utf8), undefined, JSON.stringify(name));
  }
  for (const mime of ["text", "text/ plain"]) {
    const b = clip.encodeFiles([file("a", mime, [])], utf8);
    assert.equal(clip.decodeFiles(b, utf8), undefined, mime);
  }
  const malformed = clip.encodeFiles([file("ab", "text/plain", [])], utf8);
  malformed[8] = 0xff;
  assert.equal(clip.decodeFiles(malformed, utf8), undefined);
});

test("text, HTML and web links go as the first record's text", () => {
  const text = clip.choose(
    [item({ mime: "text/plain", text: "hi" })],
    notImage,
  );
  assert.equal(text.kind, clip.TEXT);
  assert.equal(text.text, "hi");
  const html = clip.choose(
    [item({ mime: "text/html", text: "bold" })],
    notImage,
  );
  assert.equal(html.text, "bold");
  const link = clip.choose(
    [
      item({
        mime: "text/uri",
        uri: "https://example.org",
        text: "https://example.org",
      }),
    ],
    notImage,
  );
  assert.equal(link.kind, clip.TEXT);
  assert.equal(clip.choose([], notImage).kind, 0);
  assert.equal(clip.choose([item({ mime: "text/plain" })], notImage).kind, 0);
});

test("a PixelMap is an image", () => {
  const c = clip.choose([item({ mime: "pixelMap", pixelMap: true })], notImage);
  assert.equal(c.kind, clip.IMAGE);
  assert.ok(c.pixelMap);
});

test("file URIs are files, and a lone image file is an image", () => {
  const a = "file://org.example.app/data/storage/el2/base/files/a.txt";
  const b = "file://org.example.app/data/storage/el2/base/files/b.png";
  const isPng = (u) => u.endsWith(".png");
  const two = clip.choose(
    [item({ mime: "text/uri", uri: a }), item({ mime: "text/uri", uri: b })],
    isPng,
  );
  assert.equal(two.kind, clip.FILES);
  assert.deepEqual(two.uris, [a, b]);
  const one = clip.choose([item({ mime: "text/uri", uri: b })], isPng);
  assert.equal(one.kind, clip.IMAGE);
  assert.ok(!one.pixelMap);
  assert.deepEqual(one.uris, [b]);
  // A file next to text is the text.
  const mixed = clip.choose(
    [
      item({ mime: "text/plain", text: "hi" }),
      item({ mime: "text/uri", uri: a }),
    ],
    isPng,
  );
  assert.equal(mixed.kind, clip.TEXT);
});

test("at most 16 files go to the desktop", () => {
  const items = Array.from({ length: 20 }, (_, i) =>
    item({ mime: "text/uri", uri: `file://org.example.app/f${i}` }),
  );
  assert.equal(clip.choose(items, notImage).uris.length, clip.MAX_FILES);
});

test("a desktop clip and a repeated update don't go back to the desktop", () => {
  const echo = new clip.Echo();
  assert.ok(!echo.outgoing(clip.TAG, "text a"));
  assert.ok(echo.outgoing("", "text a"));
  assert.ok(!echo.outgoing("", "text a"));
  assert.ok(echo.outgoing("", "text b"));
  // After a desktop clip, copying b again in OpenHarmony sends it again.
  echo.incoming();
  assert.ok(echo.outgoing("", "text b"));
});

test("the identity hash changes with the content", () => {
  assert.equal(
    clip.hash(Uint8Array.from([1, 2])),
    clip.hash(Uint8Array.from([1, 2])),
  );
  assert.notEqual(
    clip.hash(Uint8Array.from([1, 2])),
    clip.hash(Uint8Array.from([2, 1])),
  );
});
