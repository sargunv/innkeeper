// The apps that can open a batch of files, and the messages that a failed
// open sends back to the desktop. This module uses no OpenHarmony API, so its
// tests run under Node.

// An ability that views files. It is a class, so other modules can import
// it under Node.
export class Target {
  bundleName: string = '';
  moduleName: string = '';
  abilityName: string = '';
}

// The fields of a bundle manager AbilityInfo that this module reads.
export class Ability {
  bundleName: string = '';
  moduleName: string = '';
  name: string = '';
  exported: boolean = false;
}

function key(t: Target): string {
  return `${t.bundleName}/${t.moduleName}/${t.abilityName}`;
}

// The abilities that open every file of a batch. `matches` holds the bundle
// manager's matches for each of the batch's MIME types. An ability must take
// every type, as Android's chooser also requires. Abilities of `own`, the
// caller's bundle, and private abilities don't count.
export function targets(matches: Ability[][], own: string): Target[] {
  if (matches.length === 0) {
    return [];
  }
  const all = matches.map((m: Ability[]) => new Set<string>(m.map((a: Ability) => key(toTarget(a)))));
  const seen = new Set<string>();
  const out: Target[] = [];
  for (const a of matches[0]) {
    const t = toTarget(a);
    const k = key(t);
    if (a.exported && a.bundleName !== own && !seen.has(k) && all.every((s: Set<string>) => s.has(k))) {
      seen.add(k);
      out.push(t);
    }
  }
  return out;
}

function toTarget(a: Ability): Target {
  return { bundleName: a.bundleName, moduleName: a.moduleName, abilityName: a.name };
}

// The distinct MIME types of a batch, in order. A file with no known type
// counts as ''.
export function distinctTypes(types: string[]): string[] {
  return types.filter((t: string, i: number) => types.indexOf(t) === i);
}

// The files of a batch, as an error message names them.
function files(types: string[]): string {
  const d = distinctTypes(types);
  return d.length === 1 && d[0] !== '' ? `${d[0]} files` : 'these files';
}

// The message for a batch that no ability opens. `bundle` is the app that
// `run` names, or '' for `open`.
export function noTarget(bundle: string, types: string[]): string {
  return bundle === '' ? `No OpenHarmony app opens ${files(types)}` : `${bundle} doesn't open ${files(types)}`;
}

// The bundle manager's and the ability manager's error codes that the
// messages name.
const BUNDLE_NOT_FOUND = 17700001;
export const ABILITY_NOT_FOUND = 17700003;
const BUNDLE_DISABLED = 17700026;
const ABILITY_DISABLED = 17700029;
const TARGET_NOT_FOUND = 16000001;
const INVISIBLE = 16000004;
const NO_MATCH = 16000019;

// The message for an error that opening a batch threw. `code` is a
// BusinessError's code, or undefined for an error of this app, whose message
// is already plain. `bundle` is the app that the files were for, or '' when
// no app was chosen yet.
export function openError(code: number | undefined, message: string, bundle: string, types: string[]): string {
  if (code === undefined) {
    return message;
  }
  const app = bundle === '' ? 'The app' : bundle;
  switch (code) {
    case BUNDLE_NOT_FOUND:
      return bundle === '' ? noTarget(bundle, types) : `${bundle} isn't installed`;
    case ABILITY_NOT_FOUND:
    case TARGET_NOT_FOUND:
    case NO_MATCH:
      return noTarget(bundle, types);
    case BUNDLE_DISABLED:
    case ABILITY_DISABLED:
      return `${app} is disabled`;
    case INVISIBLE:
      return `${app} doesn't let other apps open files`;
    default:
      return `OpenHarmony couldn't open the files (${code} ${message})`;
  }
}

// The reply for a batch of `n` files that opened in `bundle`, or that went to
// the app chooser when `bundle` is ''.
export function opened(n: number, bundle: string): string {
  const what = `${n} ${n === 1 ? 'file' : 'files'}`;
  return bundle === '' ? `showing the app chooser for ${what}` : `opened ${what} in ${bundle}`;
}
