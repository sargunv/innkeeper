// Session1's `launch` window command with documents grants
// (host/core/src/app_settings.rs), which `innkeeper run <bundle> <file>` and
// `innkeeper open` send. This module uses no OpenHarmony API, so its tests run
// under Node.

// The bundle that `innkeeper open` names, which asks OpenHarmony to choose the
// app.
export const CHOOSER = 'dev.sargunv.innkeeper.system';

// Files to open in an app, or in the app that OpenHarmony's chooser picks
// when `bundle` is empty.
export interface FileLaunch {
  bundle: string;
  grants: string[];
}

// Reads a relayed `launch` command with at least one grant. It returns
// undefined for any other command and for a malformed grant.
export function fileLaunch(json: string): FileLaunch | undefined {
  let c: Record<string, Object> = {};
  try {
    c = JSON.parse(json) as Record<string, Object>;
  } catch (e) {
    return undefined;
  }
  if (c === null || typeof c !== 'object' || c['action'] !== 'launch') {
    return undefined;
  }
  const bundle = c['package'];
  const grants = c['grants'];
  if (typeof bundle !== 'string' || !Array.isArray(grants) || grants.length === 0) {
    return undefined;
  }
  const ids: string[] = [];
  for (const g of grants as Object[]) {
    if (typeof g !== 'string' || !/^[0-9a-f]{16}$/.test(g as string)) {
      return undefined;
    }
    ids.push(g as string);
  }
  return { bundle: bundle === CHOOSER ? '' : bundle as string, grants: ids };
}

class WindowResponse {
  error: string = '';
  reply: string | null = null;
}

// The window response that the bridge relays to Session1's caller.
export function launched(l: FileLaunch, error: string): string {
  const r = new WindowResponse();
  r.error = error;
  if (error === '') {
    const n = l.grants.length;
    r.reply = `opening ${n} ${n === 1 ? 'file' : 'files'} in ${l.bundle === '' ? 'the chosen app' : l.bundle}`;
  }
  return JSON.stringify(r);
}
