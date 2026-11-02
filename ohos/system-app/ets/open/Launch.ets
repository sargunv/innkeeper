// Session1's `launch` window command with documents grants
// (host/core/src/app_settings.rs), which `innkeeper run <bundle> <file>` and
// `innkeeper open` send. This module uses no OpenHarmony API, so its tests run
// under Node.

// The bundle that `innkeeper open` names, which asks the user to choose the
// app.
export const CHOOSER = 'dev.sargunv.innkeeper.system';

// Files to open in an app, or in the app that the user picks in Innkeeper
// User's chooser when `bundle` is empty.
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

// Innkeeper User's answer to a file launch, which it sends in the want
// parameters of a start of Innkeeper System's service.
export class Opened {
  request: number = 0;
  error: string = '';
  reply: string = '';
}

// Reads Innkeeper User's answer from a want's parameters, or returns
// undefined if they hold none.
export function openedAnswer(p: Record<string, Object> | undefined): Opened | undefined {
  if (p === undefined) {
    return undefined;
  }
  const request = p['innkeeper.opened.request'];
  const error = p['innkeeper.opened.error'];
  const reply = p['innkeeper.opened.reply'];
  if (typeof request !== 'number' || typeof error !== 'string' || typeof reply !== 'string') {
    return undefined;
  }
  const o = new Opened();
  o.request = request as number;
  o.error = error as string;
  o.reply = reply as string;
  return o;
}

class WindowResponse {
  error: string = '';
  reply: string | null = null;
}

// The window response that the bridge relays to Session1's caller. An error
// leaves no reply.
export function launched(o: Opened): string {
  const r = new WindowResponse();
  r.error = o.error;
  if (o.error === '') {
    r.reply = o.reply;
  }
  return JSON.stringify(r);
}
