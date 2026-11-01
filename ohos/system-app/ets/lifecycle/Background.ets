// The background setting's pure parts: Session1's `background` app command
// (host/core/src/app_settings.rs) and the stored set of bundles. This module
// uses no OpenHarmony API, so its tests run under Node.

// A command that allows or forbids `bundle` to run in the background.
export interface BackgroundCommand {
  bundle: string;
  enabled: boolean;
}

// Reads a relayed app command. It returns undefined for any other command,
// such as another app action or a window command.
export function backgroundCommand(json: string): BackgroundCommand | undefined {
  let c: Record<string, Object> = {};
  try {
    c = JSON.parse(json) as Record<string, Object>;
  } catch (e) {
    return undefined;
  }
  if (c === null || typeof c !== 'object' || c['action'] !== 'background') {
    return undefined;
  }
  const bundle = c['package'];
  const enabled = c['enabled'];
  if (typeof bundle !== 'string' || typeof enabled !== 'boolean') {
    return undefined;
  }
  return { bundle: bundle as string, enabled: enabled as boolean };
}

// The stored set after allowing or forbidding `bundle`, sorted.
export function withBundle(stored: string[], bundle: string, enabled: boolean): string[] {
  const s = new Set<string>(stored);
  if (enabled) {
    s.add(bundle);
  } else {
    s.delete(bundle);
  }
  return Array.from(s).sort();
}

class AppResponse {
  error: string = '';
  info: Object | null = null;
}

// The app response that the bridge relays to Session1's caller. An
// OpenHarmony response carries no app info.
export function response(error: string): string {
  const r = new AppResponse();
  r.error = error;
  return JSON.stringify(r);
}
