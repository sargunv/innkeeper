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

test("a clip of file URIs is files, and a file next to text is the text", () => {
  const a = "file://org.example.app/data/storage/el2/base/files/a.txt";
  const b = "file://org.example.app/data/storage/el2/base/files/b.png";
  const files = clip.choose([
    item({ mime: "text/uri", uri: a, text: a }),
    item({ mime: "text/uri", uri: b, text: b }),
  ]);
  assert.equal(files.kind, clip.FILES);
  assert.deepEqual(files.uris, [a, b]);
  const many = clip.choose(
    Array.from({ length: 17 }, () =>
      item({ mime: "text/uri", uri: a, text: a }),
    ),
  );
  assert.equal(many.kind, 0);
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

const utf8 = {
  encode: (s) => encoder.encode(s),
  decode: (b) => {
    try {
      return new TextDecoder("utf-8", { fatal: true }).decode(b);
    } catch {
      return undefined;
    }
  },
};

function clipFile(name, mime, bytes) {
  return Object.assign(new clip.ClipFile(), {
    name,
    mime,
    bytes: Uint8Array.from(bytes),
  });
}

test("a file clip round-trips with the bridge's layout", () => {
  const files = [
    clipFile("a.txt", "text/plain", [104, 105]),
    clipFile("é.png", "image/png", []),
  ];
  const b = clip.encodeFiles(files, utf8);
  const v = new DataView(b.buffer);
  assert.equal(v.getUint32(0, true), 2);
  assert.equal(v.getUint32(4, true), 5);
  assert.equal(new TextDecoder().decode(b.subarray(8, 13)), "a.txt");
  assert.equal(
    b.length,
    4 +
      clip.fileOverhead("a.txt", "text/plain", utf8) +
      2 +
      clip.fileOverhead("é.png", "image/png", utf8),
  );
  const back = clip.decodeFiles(b, utf8);
  assert.deepEqual(
    back.map((f) => [f.name, f.mime, [...f.bytes]]),
    [
      ["a.txt", "text/plain", [104, 105]],
      ["é.png", "image/png", []],
    ],
  );
});

test("a file clip that breaks the format or the caps is refused", () => {
  const good = clip.encodeFiles([clipFile("a.txt", "text/plain", [1])], utf8);
  assert.equal(
    clip.decodeFiles(good.subarray(0, good.length - 1), utf8),
    undefined,
  );
  const extra = new Uint8Array(good.length + 1);
  extra.set(good);
  assert.equal(clip.decodeFiles(extra, utf8), undefined);
  assert.equal(clip.decodeFiles(new Uint8Array(4), utf8), undefined);
  for (const [name, mime] of [
    ["", "text/plain"],
    ["..", "text/plain"],
    ["a/b", "text/plain"],
    ["a\\b", "text/plain"],
    ["a", "text"],
    ["a", "text/ plain"],
  ]) {
    assert.equal(
      clip.decodeFiles(
        clip.encodeFiles([clipFile(name, mime, [])], utf8),
        utf8,
      ),
      undefined,
      `${name} ${mime}`,
    );
  }
  const seventeen = Array.from({ length: 17 }, () => clipFile("a", "a/b", []));
  assert.equal(
    clip.decodeFiles(clip.encodeFiles(seventeen, utf8), utf8),
    undefined,
  );
  const big = new Uint8Array(16);
  new DataView(big.buffer).setUint32(0, 1, true);
  assert.equal(clip.decodeFiles(big, utf8), undefined);
  const latin1 = clip.encodeFiles([clipFile("a", "a/b", [])], utf8);
  latin1[8] = 0xff;
  assert.equal(clip.decodeFiles(latin1, utf8), undefined);
});

test("a file URI names its file, and its MIME type falls back to the name's", () => {
  assert.equal(
    clip.uriName(
      "file://org.example.app/data/storage/el2/base/files/My%20Notes.txt",
    ),
    "My Notes.txt",
  );
  assert.equal(
    clip.uriName("file://org.example.app/data/storage/el2/base/files/"),
    "clipboard-file",
  );
  assert.equal(
    clip.uriName("file://org.example.app/a/%2E%2E"),
    "clipboard-file",
  );
  assert.equal(clip.uriName("file://org.example.app/a/%E0"), "%E0");
  assert.equal(clip.fileMime("image/png", "text/plain"), "image/png");
  assert.equal(clip.fileMime("text/uri", "text/plain"), "text/plain");
  assert.equal(clip.fileMime("text/uri", ""), "application/octet-stream");
});
