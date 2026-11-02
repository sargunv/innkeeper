// The bridge's HANDOFF request (host/bridge/src/system/proto.rs) for a link,
// which opens on the desktop. This module uses no OpenHarmony API, so its
// tests run under Node.

export const HANDOFF = 0x0309;
// HANDOFF's action that opens a URI.
const URI = 1;

// The link schemes that the desktop opens, as Android's system app lists
// them. The skills of DesktopAbility in module.json name the same schemes.
export const LINK_SCHEMES = [
  'http', 'https', 'mailto', 'tel', 'sms', 'smsto', 'geo', 'market', 'magnet', 'webcal', 'webcals', 'ftp', 'ftps',
];

// Whether a want's URI is a link that the desktop opens.
export function isLink(uri: string): boolean {
  const colon = uri.indexOf(':');
  return colon > 0 && LINK_SCHEMES.includes(uri.substring(0, colon).toLowerCase());
}

// The request's body: `u32 window`, `u8 action`, `u8 files` (0), `u8 ask`
// (0), `u8 0`, `u32 len`. OpenHarmony draws one window, whose id the bridge
// takes as 0.
export function handoffBody(len: number): ArrayBuffer {
  const b = new ArrayBuffer(12);
  const v = new DataView(b);
  v.setUint8(4, URI);
  v.setUint32(8, len, true);
  return b;
}

// The metadata blob: `u32 version` (1), the link and an empty subject, each a
// `u32` length and UTF-8.
export function handoffMetadata(link: Uint8Array): Uint8Array {
  const out = new Uint8Array(12 + link.length);
  const v = new DataView(out.buffer);
  v.setUint32(0, 1, true);
  v.setUint32(4, link.length, true);
  out.set(link, 8);
  return out;
}
