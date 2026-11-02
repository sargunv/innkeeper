// What the app selector's "always" checkbox remembers. This module uses no
// OpenHarmony API, so its tests run under Node.

const VIEW = 'ohos.want.action.viewData';

// The type that the bundle manager looks up for a default app when an app
// opens `uri` with MIME type `type` and `action`, or '' when it looks up none
// that "always" can set. The bundle manager reads the type of a view of a
// local file: the want's MIME type, or else the URI's extension. A MIME type
// with a wildcard can't be a default's type. Links and other actions have
// their own defaults, such as the browser.
export function defaultType(action: string, type: string, uri: string): string {
  if (action !== VIEW) {
    return '';
  }
  const scheme = /^([A-Za-z][A-Za-z0-9+.-]*):/.exec(uri);
  if (scheme !== null && scheme[1] !== 'file' && scheme[1] !== 'content') {
    return '';
  }
  if (type !== '') {
    return /^[^/*\s]+\/[^/*\s]+$/.test(type) ? type : '';
  }
  const path = uri.replace(/[?#].*$/, '');
  const name = path.slice(path.lastIndexOf('/') + 1);
  const dot = name.lastIndexOf('.');
  return dot > 0 && dot < name.length - 1 ? name.slice(dot).toLowerCase() : '';
}

// The types that the selector made `bundle` the default for, sorted, from its
// record, which maps each type to a bundle.
export function typesOf(record: Record<string, Object>, bundle: string): string[] {
  return Object.keys(record).filter((t: string) => record[t] === bundle).sort();
}

// Innkeeper System's request for the defaults of `bundle`
// (ets/service/PerUser.ets in Innkeeper System). With `clear`, Innkeeper User
// resets them first.
export interface DefaultsRequest {
  request: number;
  bundle: string;
  clear: boolean;
}

// Reads a defaults request from a want's parameters, or returns undefined if
// they hold none.
export function defaultsRequest(p: Record<string, Object> | undefined): DefaultsRequest | undefined {
  if (p === undefined) {
    return undefined;
  }
  const request = p['innkeeper.request'];
  const bundle = p['innkeeper.defaults.bundle'];
  const clear = p['innkeeper.defaults.clear'];
  if (typeof request !== 'number' || typeof bundle !== 'string' || bundle === '' || typeof clear !== 'boolean') {
    return undefined;
  }
  return { request: request as number, bundle: bundle as string, clear: clear as boolean };
}
