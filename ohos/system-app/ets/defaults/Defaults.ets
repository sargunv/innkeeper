// Session1's `info` and `clear-defaults` app commands
// (host/core/src/app_settings.rs), which `innkeeper app info` and
// `innkeeper app clear-defaults` send. Innkeeper User's app selector sets an
// app as the default for a type, and OpenHarmony's Settings has no page that
// resets it. Innkeeper User answers these commands, because it records the
// defaults that its selector set. This module uses no OpenHarmony API, so its
// tests run under Node.

import { Answer } from '../service/Answer';

// A command that lists the default types of `bundle`, after it resets them
// with `clear`.
export interface DefaultsCommand {
  bundle: string;
  clear: boolean;
}

// Reads a relayed app command. It returns undefined for any other command.
export function defaultsCommand(json: string): DefaultsCommand | undefined {
  let c: Record<string, Object> = {};
  try {
    c = JSON.parse(json) as Record<string, Object>;
  } catch (e) {
    return undefined;
  }
  if (c === null || typeof c !== 'object') {
    return undefined;
  }
  const action = c['action'];
  const bundle = c['package'];
  if ((action !== 'info' && action !== 'clear-defaults') || typeof bundle !== 'string') {
    return undefined;
  }
  return { bundle: bundle as string, clear: action === 'clear-defaults' };
}

class DefaultsResponse {
  error: string = '';
  info: Object | null = null;
  defaults: string[] | null = null;
}

// The app response that the bridge relays to Session1's caller, from
// Innkeeper User's answer, whose reply is a JSON array of types. An error
// leaves no defaults.
export function defaultsResponse(a: Answer): string {
  const r = new DefaultsResponse();
  r.error = a.error;
  if (a.error === '') {
    try {
      const types = JSON.parse(a.reply) as Object;
      if (Array.isArray(types) && (types as Object[]).every((t: Object) => typeof t === 'string')) {
        r.defaults = types as string[];
      }
    } catch (e) {
      // The check below reports it.
    }
    if (r.defaults === null) {
      r.error = "Innkeeper User's answer has no list of types";
    }
  }
  return JSON.stringify(r);
}
