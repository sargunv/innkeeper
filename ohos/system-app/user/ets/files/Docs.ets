// The documents service's OPEN and STAT messages
// (host/bridge/src/documents/proto.rs), and the want that opens the copies.
// This module uses no OpenHarmony API, so its tests run under Node.

import { Target } from './Targets';

export const OPEN = 0x0402;
export const STAT = 0x0403;
// OPEN's read mode.
const READ = 1;
// A STAT reply's kind for a regular file.
const FILE = 0;
// OpenHarmony's view action (wantConstant.Action.ACTION_VIEW_DATA) and the
// want parameter that lists every file (wantConstant.Params.ABILITY_PARAMS_STREAM).
export const VIEW = 'ohos.want.action.viewData';
export const STREAM = 'ability.params.stream';

// The body of a request about a whole file grant: `u64 grant`, then the
// request's own fields, then an empty path.
function grantBody(grant: string, open: boolean): ArrayBuffer {
  const b = new ArrayBuffer(open ? 11 : 10);
  const v = new DataView(b);
  v.setBigUint64(0, BigInt(`0x${grant}`), true);
  if (open) {
    v.setUint8(8, READ);
  }
  return b;
}

export function openBody(grant: string): ArrayBuffer {
  return grantBody(grant, true);
}

export function statBody(grant: string): ArrayBuffer {
  return grantBody(grant, false);
}

// The file name in a STAT reply: `u8 kind`, `u64 size`, `i64 mtime_ms`,
// `u8 writable`, `u16 len` and the UTF-8 name. It returns undefined for a
// folder or a malformed reply.
export function statName(body: ArrayBuffer, decode: (b: Uint8Array) => string | undefined): string | undefined {
  if (body.byteLength < 20) {
    return undefined;
  }
  const v = new DataView(body);
  const len = v.getUint16(18, true);
  if (v.getUint8(0) !== FILE || body.byteLength !== 20 + len) {
    return undefined;
  }
  const name = decode(new Uint8Array(body, 20, len));
  return name !== undefined && safeName(name) ? name : undefined;
}

// Whether `name` can name a file in a folder, as the bridge also checks.
export function safeName(name: string): boolean {
  return name.length > 0 && name !== '.' && name !== '..' && !name.includes('/') && !name.includes('\0');
}

// The file name's extension with its dot, such as '.txt', or '' for none.
export function extension(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot > 0 ? name.substring(dot).toLowerCase() : '';
}

// The chooser's want parameters (chooser/ChooserAbility.ets): the copies'
// URIs, their MIME types and the targets as JSON.
export const CHOOSE_URIS = 'innkeeper.choose.uris';
export const CHOOSE_TYPES = 'innkeeper.choose.types';
export const CHOOSE_TARGETS = 'innkeeper.choose.targets';

export class OpenWant {
  bundleName?: string;
  moduleName?: string;
  abilityName?: string;
  action: string = VIEW;
  uri: string = '';
  type?: string;
  flags: number = 0;
  parameters?: Record<string, Object>;
}

// The want that views `uris` with the read grant `flag`. An empty bundle
// leaves the choice of app to OpenHarmony, which asks the user when several
// apps match. Every URI also goes in the stream parameter, so an app that
// takes several files gets all of them.
export function openWant(bundle: string, uris: string[], type: string, flag: number): OpenWant {
  const w = new OpenWant();
  if (bundle !== '') {
    w.bundleName = bundle;
  }
  w.uri = uris[0];
  if (type !== '') {
    w.type = type;
  }
  w.flags = flag;
  if (uris.length > 1) {
    const p: Record<string, Object> = {};
    p[STREAM] = uris;
    w.parameters = p;
  }
  return w;
}

// The want that opens `uris` in ability `t`. Their MIME types are `types`,
// and the want carries the first one.
export function targetWant(t: Target, uris: string[], types: string[], flag: number): OpenWant {
  const w = openWant(t.bundleName, uris, types[0] ?? '', flag);
  w.moduleName = t.moduleName;
  w.abilityName = t.abilityName;
  return w;
}
