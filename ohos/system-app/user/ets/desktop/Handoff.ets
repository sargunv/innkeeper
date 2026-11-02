// The bridge's HANDOFF request (host/bridge/src/system/proto.rs) for a link
// or a file, which opens on the desktop. This module uses no OpenHarmony API,
// so its tests run under Node.

export const HANDOFF = 0x0309;
// HANDOFF's actions that open a URI and open files.
export const URI = 1;
export const OPEN = 2;

// The longest file name that the bridge takes, in UTF-8 bytes.
const MAX_NAME = 255;
// The name of a file whose URI names none.
const FALLBACK_NAME = 'Shared file';

// The link schemes that the desktop opens, as Android's system app lists
// them. The skills of DesktopAbility in module.json name the same schemes.
export const LINK_SCHEMES = [
  'http', 'https', 'mailto', 'tel', 'sms', 'smsto', 'geo', 'market', 'magnet', 'webcal', 'webcals', 'ftp', 'ftps',
];

function scheme(uri: string): string {
  const colon = uri.indexOf(':');
  return colon > 0 ? uri.substring(0, colon).toLowerCase() : '';
}

// Whether a want's URI is a link that the desktop opens.
export function isLink(uri: string): boolean {
  return LINK_SCHEMES.includes(scheme(uri));
}

// Whether a want's URI is a file that the desktop opens.
export function isFile(uri: string): boolean {
  return scheme(uri) === 'file';
}

// The file's name from its URI's last path segment, which the desktop app
// shows. A name that can't name a file becomes "Shared file".
export function fileName(uri: string): string {
  let path = uri.substring(uri.indexOf(':') + 1);
  for (const end of ['?', '#']) {
    const i = path.indexOf(end);
    if (i >= 0) {
      path = path.substring(0, i);
    }
  }
  let name = path.substring(path.lastIndexOf('/') + 1);
  try {
    name = decodeURIComponent(name);
  } catch (e) {
    // The segment isn't percent-encoded UTF-8, so it stays as it is.
  }
  let clean = '';
  for (const c of name) {
    const code = c.codePointAt(0) ?? 0;
    if (code >= 0x20 && code !== 0x7f && !(code >= 0x80 && code < 0xa0)) {
      clean += c === '/' || c === '\\' ? '_' : c;
    }
  }
  return clean === '' || clean === '.' || clean === '..' ? FALLBACK_NAME : clean;
}

// The first `max` bytes of UTF-8 `b`, cut before a character that doesn't
// fit.
export function truncateUtf8(b: Uint8Array, max: number): Uint8Array {
  if (b.length <= max) {
    return b;
  }
  let end = max;
  while (end > 0 && (b[end] & 0xc0) === 0x80) {
    end--;
  }
  return b.subarray(0, end);
}

// The request's body: `u32 window`, `u8 action`, `u8 files`, `u8 ask` (0),
// `u8 0`, `u32 len`. OpenHarmony draws one window, whose id the bridge takes
// as 0. The desktop opens a file in its default app without asking, as
// Android's HandoffActivity does.
export function handoffBody(action: number, files: number, len: number): ArrayBuffer {
  const b = new ArrayBuffer(12);
  const v = new DataView(b);
  v.setUint8(4, action);
  v.setUint8(5, files);
  v.setUint32(8, len, true);
  return b;
}

// The metadata blob: `u32 version` (1), the text, an empty subject and one
// name per file, each a `u32` length and UTF-8. A link handoff's text is the
// link, and a file handoff's text is empty. Names longer than the bridge
// takes are cut.
export function handoffMetadata(text: Uint8Array, names: Uint8Array[]): Uint8Array {
  const cut = names.map((n: Uint8Array) => truncateUtf8(n, MAX_NAME));
  const size = cut.reduce((n: number, b: Uint8Array) => n + 4 + b.length, 12 + text.length);
  const out = new Uint8Array(size);
  const v = new DataView(out.buffer);
  let at = 0;
  const put = (b: Uint8Array): void => {
    v.setUint32(at, b.length, true);
    out.set(b, at + 4);
    at += 4 + b.length;
  };
  v.setUint32(0, 1, true);
  at = 4;
  put(text);
  put(new Uint8Array(0));
  cut.forEach(put);
  return out;
}
