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
