// Host tests for Innkeeper User's ets/clipboard/Clip.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const clip = await load("Clip", "clipboard");

const encoder = new TextEncoder();

function item(fields) {
  return Object.assign(new clip.Item(), fields);
}

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
    [...new Uint8Array(clip.encodeHeader(clip.IMAGE, 0x01020304))],
    [2, 0, 0, 0, 4, 3, 2, 1],
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

test("text, HTML and web links go as the first record's text", () => {
  const text = clip.choose([item({ mime: "text/plain", text: "hi" })]);
  assert.equal(text.kind, clip.TEXT);
  assert.equal(text.text, "hi");
  const html = clip.choose([item({ mime: "text/html", text: "bold" })]);
  assert.equal(html.text, "bold");
  const link = clip.choose([
    item({
      mime: "text/uri",
      uri: "https://example.org",
      text: "https://example.org",
    }),
  ]);
  assert.equal(link.kind, clip.TEXT);
  assert.equal(clip.choose([]).kind, 0);
  assert.equal(clip.choose([item({ mime: "text/plain" })]).kind, 0);
});

test("a PixelMap is an image", () => {
  const c = clip.choose([item({ mime: "pixelMap", pixelMap: true })]);
  assert.equal(c.kind, clip.IMAGE);
});

test("a clip of file URIs sends nothing, and a file next to text is the text", () => {
  const a = "file://org.example.app/data/storage/el2/base/files/a.txt";
  const b = "file://org.example.app/data/storage/el2/base/files/b.png";
  const files = clip.choose([
    item({ mime: "text/uri", uri: a, text: a }),
    item({ mime: "text/uri", uri: b, text: b }),
  ]);
  assert.equal(files.kind, 0);
  const mixed = clip.choose([
    item({ mime: "text/plain", text: "hi" }),
    item({ mime: "text/uri", uri: a, text: a }),
  ]);
  assert.equal(mixed.kind, clip.TEXT);
  assert.equal(mixed.text, "hi");
});

test("a desktop clip and a repeated update don't go back to the desktop", () => {
  const echo = new clip.Echo();
  assert.ok(!echo.outgoing(clip.TAG, "text a"));
  assert.ok(echo.outgoing("", "text a"));
  assert.ok(!echo.outgoing("", "text a"));
  assert.ok(echo.outgoing("", "text b"));
  // After a desktop clip, copying b again in OpenHarmony sends it again.
  echo.incoming("text c");
  assert.ok(echo.outgoing("", "text b"));
});

test("a desktop clip doesn't go back when the pasteboard drops its tag", () => {
  const echo = new clip.Echo();
  echo.incoming("text a");
  assert.ok(!echo.outgoing("", "text a"));
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
