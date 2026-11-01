// The clipboard's messages and the choice of what to send, without
// OpenHarmony types, so ohos/system-app/tests runs them under Node.
// HostClipboard.ets reads and writes OpenHarmony's pasteboard with them.
//
// host/bridge/src/system/proto.rs documents the formats. CLIP_SET and CLIP
// carry a header and a sealed memfd. Kind 1 is raw UTF-8, kind 2 is
// `u16 width, u16 height` and RGBA rows, and kind 3 is a list of files.

export const TEXT = 1;
export const IMAGE = 2;
export const FILES = 3;

// The bridge's caps (limits in proto.rs and files.rs), which Android's
// system app also applies.
export const MAX_TEXT = 1 << 20;
export const MAX_SIDE = 2048;
export const MAX_FILES = 16;
export const MAX_FILE = (16 << 20) - 1024;
export const MAX_SELECTION = 16 << 20;
const MAX_NAME = 255;

// UTF-8 conversion. OpenHarmony's util.TextEncoder and util.TextDecoder do
// it in the app, and Node's do it in the tests.
export interface Utf8 {
  encode(s: string): Uint8Array;
  // Returns undefined for malformed UTF-8.
  decode(b: Uint8Array): string | undefined;
}

export class ClipHeader {
  kind: number = 0;
  sensitive: boolean = false;
  len: number = 0;
}

// Reads a CLIP event's body, `u8 kind, u8 sensitive, u16 0, u32 len`.
export function decodeHeader(body: ArrayBuffer): ClipHeader | undefined {
  if (body.byteLength !== 8) {
    return undefined;
  }
  const v = new DataView(body);
  const h = new ClipHeader();
  h.kind = v.getUint8(0);
  h.sensitive = v.getUint8(1) !== 0;
  h.len = v.getUint32(4, true);
  return h;
}

// A CLIP_SET body for `len` bytes of `kind`. OpenHarmony has no sensitive
// flag for a clip, so no clip goes out as sensitive.
export function encodeHeader(kind: number, len: number): ArrayBuffer {
  const b = new ArrayBuffer(8);
  const v = new DataView(b);
  v.setUint8(0, kind);
  v.setUint32(4, len, true);
  return b;
}

// The longest prefix of `b` that fits in `max` bytes and ends on a whole
// UTF-8 sequence.
export function truncateUtf8(b: Uint8Array, max: number): Uint8Array {
  if (b.length <= max) {
    return b;
  }
  let end = max;
  // Back up over continuation bytes to the start of the cut sequence.
  while (end > 0 && (b[end] & 0xc0) === 0x80) {
    end--;
  }
  return b.subarray(0, end);
}

export class Rgba {
  width: number = 0;
  height: number = 0;
  pixels: Uint8Array = new Uint8Array(0);
}

// Reads a kind 2 blob, or returns undefined if its size is wrong.
export function decodeImage(b: Uint8Array): Rgba | undefined {
  if (b.length < 4) {
    return undefined;
  }
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const img = new Rgba();
  img.width = v.getUint16(0, true);
  img.height = v.getUint16(2, true);
  if (img.width === 0 || img.height === 0 || img.width > MAX_SIDE || img.height > MAX_SIDE ||
    b.length !== 4 + img.width * img.height * 4) {
    return undefined;
  }
  img.pixels = b.subarray(4);
  return img;
}

export function encodeImage(img: Rgba): Uint8Array {
  const out = new Uint8Array(4 + img.pixels.length);
  const v = new DataView(out.buffer);
  v.setUint16(0, img.width, true);
  v.setUint16(2, img.height, true);
  out.set(img.pixels, 4);
  return out;
}

// The pixel layouts that a PixelMap can hold and the bridge can take.
export const RGBA = 0;
export const BGRA = 1;

// Copies a PixelMap's rows, which may be padded to `stride` bytes, to
// straight RGBA. A PixelMap's alpha can be premultiplied, and the bridge's
// images aren't.
export function toRgba(src: Uint8Array, width: number, height: number, stride: number, layout: number,
  premultiplied: boolean): Rgba {
  const img = new Rgba();
  img.width = width;
  img.height = height;
  img.pixels = new Uint8Array(width * height * 4);
  const out = img.pixels;
  const r = layout === BGRA ? 2 : 0;
  const b = layout === BGRA ? 0 : 2;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = y * stride + x * 4;
      const o = (y * width + x) * 4;
      const a = src[i + 3];
      if (premultiplied && a !== 0 && a !== 255) {
        out[o] = Math.min(255, Math.round(src[i + r] * 255 / a));
        out[o + 1] = Math.min(255, Math.round(src[i + 1] * 255 / a));
        out[o + 2] = Math.min(255, Math.round(src[i + b] * 255 / a));
      } else {
        out[o] = src[i + r];
        out[o + 1] = src[i + 1];
        out[o + 2] = src[i + b];
      }
      out[o + 3] = a;
    }
  }
  return img;
}

// The scale that fits an image in MAX_SIDE on each side, or 1.
export function fitScale(width: number, height: number): number {
  const side = Math.max(width, height);
  return side > MAX_SIDE ? MAX_SIDE / side : 1;
}

export class ClipFile {
  name: string = '';
  mime: string = '';
  bytes: Uint8Array = new Uint8Array(0);
}

// Whether the bridge takes `name` as a file's name: 1 to 255 bytes with no
// slash, no backslash and no control character, and not `.` or `..`.
export function validName(name: string, utf8: Utf8): boolean {
  const n = utf8.encode(name).length;
  return n > 0 && n <= MAX_NAME && name !== '.' && name !== '..' && !/[\/\\\x00-\x1f\x7f]/.test(name);
}

export function validMime(mime: string): boolean {
  return mime.length > 0 && mime.length <= MAX_NAME && mime.includes('/') && !/[\s\x00-\x1f\x7f]/.test(mime);
}

// The size of the kind 3 blob that holds `files`.
export function filesSize(files: ClipFile[], utf8: Utf8): number {
  let n = 4;
  for (const f of files) {
    n += 12 + utf8.encode(f.name).length + utf8.encode(f.mime).length + f.bytes.length;
  }
  return n;
}

// Writes a kind 3 blob: `u32 count`, then per file a `u32` length and the
// name, a `u32` length and the MIME type, and a `u32` size and the bytes.
export function encodeFiles(files: ClipFile[], utf8: Utf8): Uint8Array {
  const out = new Uint8Array(filesSize(files, utf8));
  const v = new DataView(out.buffer);
  v.setUint32(0, files.length, true);
  let at = 4;
  const put = (b: Uint8Array): void => {
    v.setUint32(at, b.length, true);
    out.set(b, at + 4);
    at += 4 + b.length;
  };
  for (const f of files) {
    put(utf8.encode(f.name));
    put(utf8.encode(f.mime));
    put(f.bytes);
  }
  return out;
}

// Reads a kind 3 blob, or returns undefined if the bridge wouldn't have
// sent it.
export function decodeFiles(b: Uint8Array, utf8: Utf8): ClipFile[] | undefined {
  if (b.length < 4 || b.length > MAX_SELECTION) {
    return undefined;
  }
  const v = new DataView(b.buffer, b.byteOffset, b.byteLength);
  let at = 0;
  const take = (max: number): Uint8Array | undefined => {
    if (at + 4 > b.length) {
      return undefined;
    }
    const n = v.getUint32(at, true);
    if (n > max || at + 4 + n > b.length) {
      return undefined;
    }
    const out = b.subarray(at + 4, at + 4 + n);
    at += 4 + n;
    return out;
  };
  const count = v.getUint32(0, true);
  at = 4;
  if (count < 1 || count > MAX_FILES) {
    return undefined;
  }
  const files: ClipFile[] = [];
  for (let i = 0; i < count; i++) {
    const name = take(MAX_NAME);
    const mime = take(MAX_NAME);
    const bytes = take(MAX_FILE);
    if (name === undefined || mime === undefined || bytes === undefined) {
      return undefined;
    }
    const f = new ClipFile();
    const n = utf8.decode(name);
    const m = utf8.decode(mime);
    if (n === undefined || m === undefined || !validName(n, utf8) || !validMime(m)) {
      return undefined;
    }
    f.name = n;
    f.mime = m;
    f.bytes = bytes;
    files.push(f);
  }
  return at === b.length ? files : undefined;
}

// What one pasteboard record holds, as HostClipboard.ets reads it.
export class Item {
  // The record's MIME type, such as text/plain, text/html, text/uri or
  // pixelMap.
  mime: string = '';
  text: string = '';
  uri: string = '';
  pixelMap: boolean = false;
}

// What goes to the desktop for a pasteboard's records.
export class Choice {
  kind: number = 0;
  text: string = '';
  // The file URIs, for FILES and for an image file.
  uris: string[] = [];
  // Whether the image is the first record's PixelMap.
  pixelMap: boolean = false;
}

const PIXEL_MAP = 'pixelMap';

// Chooses what to send, as Android's ClipboardSync does with a clip. A
// PixelMap in the first record is an image. File URIs are files, and a lone
// file with an image extension is an image. Anything else goes as the first
// record's text. `imageFile` says whether a file name is an image's. A kind
// of 0 sends nothing.
export function choose(items: Item[], imageFile: (uri: string) => boolean): Choice {
  const c = new Choice();
  if (items.length === 0) {
    return c;
  }
  const first = items[0];
  if (first.mime === PIXEL_MAP || first.pixelMap) {
    c.kind = IMAGE;
    c.pixelMap = true;
    return c;
  }
  const files = items.filter((i: Item) => i.uri.startsWith('file://')).map((i: Item) => i.uri);
  if (files.length > 0 && files.length === items.length) {
    c.uris = files.slice(0, MAX_FILES);
    c.kind = files.length === 1 && imageFile(files[0]) ? IMAGE : FILES;
    return c;
  }
  if (first.text.length > 0) {
    c.kind = TEXT;
    c.text = first.text;
  }
  return c;
}

// A short identity for content, so a repeated update event doesn't send it
// twice. FNV-1a over the bytes.
export function hash(b: Uint8Array): string {
  let h = 0x811c9dc5;
  for (let i = 0; i < b.length; i++) {
    h = Math.imul(h ^ b[i], 0x01000193) >>> 0;
  }
  return h.toString(16);
}

// Keeps each side's change from coming back. Android's ClipboardSync does
// the same with what each side last got from the other.
//
// The app marks each clip that it writes for the desktop with TAG, and an
// update event for a clip with that tag sends nothing. OpenHarmony can send
// more than one update event for one change, so the app also skips a clip
// that it has just sent.
export const TAG = 'dev.sargunv.innkeeper.desktop';

export class Echo {
  private sent: string = '';

  // Whether the pasteboard's clip, with `tag` and identity `key`, goes to the
  // desktop. A true answer records it as sent.
  outgoing(tag: string, key: string): boolean {
    if (tag === TAG || key === this.sent) {
      return false;
    }
    this.sent = key;
    return true;
  }

  // Notes a clip from the desktop. The desktop now holds something else, so
  // the same OpenHarmony clip copied again goes out again.
  incoming(): void {
    this.sent = '';
  }
}
