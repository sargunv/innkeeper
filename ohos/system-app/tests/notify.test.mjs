// Host tests for ets/notify/Note.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const n = await load("Note", "notify");

function text(fields) {
  return Object.assign(new n.Text(), fields);
}

// Reads a blob as the bridge's Note::decode does.
function decode(bytes) {
  const v = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  let at = 0;
  const u8 = () => v.getUint8(at++);
  const u16 = () => ((at += 2), v.getUint16(at - 2, true));
  const u32 = () => ((at += 4), v.getUint32(at - 4, true));
  const str = () => {
    const len = u32();
    at += len;
    return new TextDecoder("utf-8", { fatal: true }).decode(
      bytes.subarray(at - len, at),
    );
  };
  const image = () => {
    const [w, h] = [u16(), u16()];
    at += w * h * 4;
    return w === 0 ? null : [w, h, [...bytes.subarray(at - w * h * 4, at)]];
  };
  const note = {
    version: u32(),
    bundle: str(),
    app: str(),
    title: str(),
    body: str(),
    flags: u8(),
    urgency: u8(),
    reserved: u16(),
    progress: ((at += 4), v.getInt32(at - 4, true)),
    actions: [],
  };
  for (let i = u8(); i > 0; i--) {
    note.actions.push([str(), u8(), str()]);
  }
  note.icon = image();
  note.picture = image();
  assert.equal(at, bytes.length, "trailing bytes");
  return note;
}

test("the expanded title and long text or lines make the title and body", () => {
  const plain = text({ title: "T", text: "body" });
  assert.equal(n.titleOf(plain), "T");
  assert.equal(n.bodyOf(plain), "body");
  const long = text({
    title: "T",
    text: "short",
    expandedTitle: "Big",
    longText: "long",
  });
  assert.equal(n.titleOf(long), "Big");
  assert.equal(n.bodyOf(long), "long");
  assert.equal(n.bodyOf(text({ text: "short", lines: ["a", "b"] })), "a\nb");
});

test("progress is a percentage, or -1 without a maximum", () => {
  assert.equal(n.percent(30, 60), 50);
  assert.equal(n.percent(150, 100), 100);
  assert.equal(n.percent(5, 0), -1);
  assert.equal(n.percent(-1, 100), -1);
});

test("slot levels and types map to urgency and sound", () => {
  // SlotLevel 1 and 2 are minimal and low. SlotType 10 is emergency
  // information.
  assert.equal(n.urgencyOf(2, 1), 0);
  assert.equal(n.urgencyOf(3, 1), 1);
  assert.equal(n.urgencyOf(4, 10), 2);
  assert.equal(n.silentOf(3, false, true), false);
  assert.equal(n.silentOf(3, true, true), true);
  assert.equal(n.silentOf(2, false, false), true);
});

test("images are scaled to fit and read as straight RGBA", () => {
  assert.equal(n.scaleFor(512, 256, 256), 0.5);
  assert.equal(n.scaleFor(64, 32, 256), 1);
  // Two BGRA pixels in rows padded to 12 bytes, premultiplied: an opaque
  // red pixel and a half-transparent green one.
  const px = new Uint8Array([0, 0, 255, 255, 0, 64, 0, 128, 9, 9, 9, 9]);
  const img = n.rgba(px.buffer, 2, 1, 12, 4, true);
  assert.deepEqual([...img.data], [255, 0, 0, 255, 0, 128, 0, 128]);
  const straight = n.rgba(px.buffer, 2, 1, 12, 3, false);
  assert.deepEqual([...straight.data], [0, 0, 255, 255, 0, 64, 0, 128]);
  // RGB_565 and a buffer shorter than its rows are left out.
  assert.equal(n.rgba(px.buffer, 2, 1, 12, 2, false), undefined);
  assert.equal(n.rgba(px.buffer, 2, 2, 12, 3, false), undefined);
});

test("text is UTF-8 cut at a character boundary, without NUL", () => {
  const enc = (s, max) => Buffer.from(n.utf8(s, max)).toString();
  assert.equal(enc("héllo 👋", 64), "héllo 👋");
  assert.equal(enc("a\0b", 64), "ab");
  // é is two bytes, so a cap of 2 keeps only "h".
  assert.equal(enc("hé", 2), "h");
  assert.equal(enc("👋👋", 7), "👋");
  assert.equal(enc("\ud800x", 64), "�x");
});

test("the blob has the bridge's layout", () => {
  const note = Object.assign(new n.Note(), {
    bundle: "org.example.chat",
    appName: "Chat",
    title: "Ada",
    body: "x".repeat(9000),
    ongoing: false,
    silent: true,
    clickable: true,
    badge: true,
    urgency: 1,
    progress: -1,
    actions: ["Read", "Reply", "Mute", "Block"].map((label, i) =>
      Object.assign(new n.Action(), { label, reply: i === 1 }),
    ),
    icon: Object.assign(new n.Rgba(), {
      width: 1,
      height: 1,
      data: new Uint8Array([1, 2, 3, 4]),
    }),
  });
  const d = decode(n.encode(note));
  assert.equal(d.version, 1);
  assert.equal(d.bundle, "org.example.chat");
  assert.equal(d.app, "Chat");
  assert.equal(d.body.length, 8192);
  // Silent, clickable and badged.
  assert.equal(d.flags, 2 | 4 | 8);
  assert.equal(d.progress, -1);
  assert.deepEqual(d.actions, [
    ["Read", 0, ""],
    ["Reply", 1, ""],
    ["Mute", 0, ""],
  ]);
  assert.deepEqual(d.icon, [1, 1, [1, 2, 3, 4]]);
  assert.equal(d.picture, null);
  // An ongoing notification gets no badge.
  note.ongoing = true;
  assert.equal(decode(n.encode(note)).flags, 1 | 2 | 4);
});

test("request and event bodies", () => {
  assert.deepEqual(
    [...new Uint8Array(n.notifyBody(3, 300))],
    [3, 0, 0, 0, 44, 1, 0, 0],
  );
  assert.deepEqual([...new Uint8Array(n.idBody(7))], [7, 0, 0, 0]);
  const ev = n.decodeEvent(
    new Uint8Array([5, 0, 0, 0, 4, 1, 0, 0, 11, 0, 0, 0]).buffer,
  );
  assert.deepEqual({ ...ev }, { id: 5, what: 4, action: 1, len: 11 });
  assert.equal(n.decodeEvent(new ArrayBuffer(8)), undefined);
  assert.equal(n.same(undefined, new Uint8Array(1)), false);
  assert.equal(n.same(new Uint8Array([1]), new Uint8Array([1])), true);
  assert.equal(n.same(new Uint8Array([1]), new Uint8Array([2])), false);
});
