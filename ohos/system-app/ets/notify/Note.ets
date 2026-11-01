// OpenHarmony notifications as the bridge's notification blobs
// (host/bridge/src/system/notify.rs, `Note`), and the desktop's events on
// them. This module imports no OpenHarmony API, so Node runs its tests
// (tests/notify.test.mjs). Notifications.ets reads the notifications and
// fills in a Note.

// host/bridge/src/system/proto.rs
export const NOTIFY = 0x0302;
export const NOTIFY_CLOSE = 0x0303;
export const SHOW_SCREEN = 0x030b;
export const NOTIFY_EVENT = 0x4302;

// What happened on the desktop, in NOTIFY_EVENT.
export const CLICKED = 1;
export const ACTION = 2;
export const DISMISSED = 3;
export const REPLY = 4;

// The bridge's caps on one notification.
const MAX_ACTIONS = 3;
export const MAX_ICON = 256;

// notificationManager.SlotLevel and SlotType values.
const LEVEL_LOW = 2;
const LEVEL_DEFAULT = 3;
const EMERGENCY_INFORMATION = 10;

// image.PixelMapFormat and image.AlphaType values.
const RGBA_8888 = 3;
const BGRA_8888 = 4;

// An image as rows of RGBA bytes without premultiplied alpha.
export class Rgba {
  width: number = 0;
  height: number = 0;
  data: Uint8Array = new Uint8Array(0);
}

export class Action {
  label: string = '';
  // The button takes the user's text.
  reply: boolean = false;
}

export class Note {
  bundle: string = '';
  appName: string = '';
  title: string = '';
  body: string = '';
  ongoing: boolean = false;
  silent: boolean = false;
  clickable: boolean = false;
  badge: boolean = false;
  // 0 low, 1 normal, 2 critical.
  urgency: number = 1;
  // -1 for none, otherwise 0 to 100.
  progress: number = -1;
  actions: Action[] = [];
  // The app's icon, which the desktop shows when it has no entry for the
  // app.
  icon: Rgba | undefined = undefined;
  // The large icon or the picture.
  picture: Rgba | undefined = undefined;
}

// The text of a notification's content. A long-text, multi-line or picture
// notification adds its expanded title and its long text or lines.
export class Text {
  title: string = '';
  text: string = '';
  expandedTitle: string = '';
  longText: string = '';
  lines: string[] = [];
}

// The title that the expanded notification shows.
export function titleOf(t: Text): string {
  return t.expandedTitle.length > 0 ? t.expandedTitle : t.title;
}

// The body that the expanded notification shows.
export function bodyOf(t: Text): string {
  if (t.longText.length > 0) {
    return t.longText;
  }
  const lines = t.lines.join('\n');
  return lines.length > 0 ? lines : t.text;
}

// `current` of `max` as a percentage, or -1 without a maximum.
export function percent(current: number, max: number): number {
  if (!(max > 0) || !(current >= 0)) {
    return -1;
  }
  return Math.min(100, Math.floor(current * 100 / max));
}

// The desktop's urgency for a slot. Low and minimal slots are low, and
// emergency information is critical, as Android's calls and alarms are.
export function urgencyOf(level: number, slotType: number): number {
  if (slotType === EMERGENCY_INFORMATION) {
    return 2;
  }
  return level <= LEVEL_LOW ? 0 : 1;
}

// An update of an alert-once notification, and every notification in a slot
// below the default level, make no sound.
export function silentOf(level: number, update: boolean, alertOnce: boolean): boolean {
  return (update && alertOnce) || level < LEVEL_DEFAULT;
}

// The scale that fits a width by height image in max by max.
export function scaleFor(width: number, height: number, max: number): number {
  return Math.min(1, max / Math.max(width, height, 1));
}

// Reads a PixelMap's pixels as RGBA. Returns undefined for a format other
// than RGBA_8888 and BGRA_8888.
export function rgba(pixels: ArrayBuffer, width: number, height: number, stride: number,
  format: number, premultiplied: boolean): Rgba | undefined {
  if ((format !== RGBA_8888 && format !== BGRA_8888) || width <= 0 || height <= 0 ||
    stride < width * 4 || pixels.byteLength < stride * height) {
    return undefined;
  }
  const src = new Uint8Array(pixels);
  const out = new Uint8Array(width * height * 4);
  const red = format === RGBA_8888 ? 0 : 2;
  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const i = y * stride + x * 4;
      const o = (y * width + x) * 4;
      const a = src[i + 3];
      let r = src[i + red];
      let g = src[i + 1];
      let b = src[i + 2 - red];
      if (premultiplied && a > 0 && a < 255) {
        r = Math.min(255, Math.round(r * 255 / a));
        g = Math.min(255, Math.round(g * 255 / a));
        b = Math.min(255, Math.round(b * 255 / a));
      }
      out[o] = r;
      out[o + 1] = g;
      out[o + 2] = b;
      out[o + 3] = a;
    }
  }
  const img = new Rgba();
  img.width = width;
  img.height = height;
  img.data = out;
  return img;
}

// Little-endian writer for the blob.
class Writer {
  bytes: number[] = [];

  u8(v: number): Writer {
    this.bytes.push(v & 0xff);
    return this;
  }

  u16(v: number): Writer {
    return this.u8(v).u8(v >>> 8);
  }

  u32(v: number): Writer {
    return this.u16(v & 0xffff).u16(v >>> 16);
  }

  // A u32 length and at most `max` bytes of UTF-8. The bridge refuses NUL
  // characters, so they are left out, and a lone surrogate becomes U+FFFD.
  text(s: string, max: number): Writer {
    const b = utf8(s, max);
    this.u32(b.length);
    b.forEach((x: number) => {
      this.bytes.push(x);
    });
    return this;
  }

  image(img: Rgba | undefined): Writer {
    if (img === undefined) {
      return this.u16(0).u16(0);
    }
    this.u16(img.width).u16(img.height);
    img.data.forEach((x: number) => {
      this.bytes.push(x);
    });
    return this;
  }
}

// The UTF-8 of `s`, cut at a character boundary to at most `max` bytes.
export function utf8(s: string, max: number): number[] {
  const out: number[] = [];
  let i = 0;
  while (i < s.length) {
    let c = s.charCodeAt(i);
    i++;
    if (c >= 0xd800 && c < 0xdc00 && i < s.length) {
      const low = s.charCodeAt(i);
      if (low >= 0xdc00 && low < 0xe000) {
        c = 0x10000 + ((c - 0xd800) << 10) + (low - 0xdc00);
        i++;
      }
    }
    if (c >= 0xd800 && c < 0xe000) {
      c = 0xfffd;
    }
    if (c === 0) {
      continue;
    }
    const n = c < 0x80 ? 1 : c < 0x800 ? 2 : c < 0x10000 ? 3 : 4;
    if (out.length + n > max) {
      break;
    }
    if (n === 1) {
      out.push(c);
    } else if (n === 2) {
      out.push(0xc0 | (c >> 6), 0x80 | (c & 0x3f));
    } else if (n === 3) {
      out.push(0xe0 | (c >> 12), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
    } else {
      out.push(0xf0 | (c >> 18), 0x80 | ((c >> 12) & 0x3f), 0x80 | ((c >> 6) & 0x3f), 0x80 | (c & 0x3f));
    }
  }
  return out;
}

// The notification blob: u32 version (1), text bundle, app name, title and
// body, u8 flags, u8 urgency, u16 0, i32 progress, u8 actions, each a text
// label, u8 reply and a text placeholder, and then the icon and the picture.
// Text is cut to the bridge's caps.
export function encode(n: Note): Uint8Array {
  const flags = (n.ongoing ? 1 : 0) | (n.silent ? 2 : 0) | (n.clickable ? 4 : 0) |
    (n.badge && !n.ongoing ? 8 : 0);
  const w = new Writer()
    .u32(1)
    .text(n.bundle, 255)
    .text(n.appName, 256)
    .text(n.title, 1024)
    .text(n.body, 8192)
    .u8(flags)
    .u8(n.urgency)
    .u16(0)
    .u32(n.progress);
  const actions = n.actions.slice(0, MAX_ACTIONS);
  w.u8(actions.length);
  actions.forEach((a: Action) => {
    // OpenHarmony's user input has no placeholder text.
    w.text(a.label, 128).u8(a.reply ? 1 : 0).text('', 128);
  });
  w.image(n.icon).image(n.picture);
  return new Uint8Array(w.bytes);
}

// NOTIFY's body: u32 id, u32 blob length.
export function notifyBody(id: number, len: number): ArrayBuffer {
  return new Uint8Array(new Writer().u32(id).u32(len).bytes).buffer;
}

// NOTIFY_CLOSE's and SHOW_SCREEN's body: u32 id.
export function idBody(id: number): ArrayBuffer {
  return new Uint8Array(new Writer().u32(id).bytes).buffer;
}

export class NoteEvent {
  id: number = 0;
  what: number = 0;
  action: number = 0;
  // The length of the reply text in the event's fd.
  len: number = 0;
}

// Reads a NOTIFY_EVENT body: u32 id, u8 what, u8 action, u16 0, u32 len.
export function decodeEvent(body: ArrayBuffer): NoteEvent | undefined {
  if (body.byteLength !== 12) {
    return undefined;
  }
  const v = new DataView(body);
  const e = new NoteEvent();
  e.id = v.getUint32(0, true);
  e.what = v.getUint8(4);
  e.action = v.getUint8(5);
  e.len = v.getUint32(8, true);
  return e;
}

// Whether two blobs hold the same bytes. Apps repost notifications that
// haven't changed, and the desktop would show each repost again.
export function same(a: Uint8Array | undefined, b: Uint8Array): boolean {
  if (a === undefined || a.length !== b.length) {
    return false;
  }
  for (let i = 0; i < a.length; i++) {
    if (a[i] !== b[i]) {
      return false;
    }
  }
  return true;
}
