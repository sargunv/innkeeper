import { defaultAppManager } from '@kit.AbilityKit';
import { BusinessError } from '@kit.BasicServicesKit';
import common from '@ohos.app.ability.common';
import preferences from '@ohos.data.preferences';
import { typesOf } from './Defaults';

const STORE = 'selector-defaults';
// defaultAppManager's errors for a type without a default app.
const NO_DEFAULT = [17700023, 17700025];

// The defaults that the app selector set, as a map from each type to the
// bundle that opens it. OpenHarmony lists default apps by type, and its
// Settings has no page that resets them, so `innkeeper app info` and
// `innkeeper app clear-defaults` read the record. The selector's extension
// and UserService run in separate processes, and each preferences instance
// caches the file, so each use reads the file again.
export class SelectorDefaults {
  private context: common.Context;

  constructor(context: common.Context) {
    this.context = context;
  }

  async remember(type: string, bundle: string): Promise<void> {
    const prefs = this.load();
    prefs.putSync(type, bundle);
    await prefs.flush();
  }

  // The types that `bundle` opens by default among those that the selector
  // set, sorted. With `clear`, it resets their defaults first. A type whose
  // default changed since is dropped from the record.
  async of(bundle: string, clear: boolean): Promise<string[]> {
    const prefs = this.load();
    const types: string[] = [];
    for (const type of typesOf(prefs.getAllSync() as Record<string, Object>, bundle)) {
      if (await defaultBundle(type) !== bundle) {
        prefs.deleteSync(type);
      } else if (clear) {
        await defaultAppManager.resetDefaultApplication(type);
        prefs.deleteSync(type);
      } else {
        types.push(type);
      }
    }
    await prefs.flush();
    return types;
  }

  private load(): preferences.Preferences {
    preferences.removePreferencesFromCacheSync(this.context, { name: STORE });
    return preferences.getPreferencesSync(this.context, { name: STORE });
  }
}

// The bundle that opens `type` by default, or '' if none does.
async function defaultBundle(type: string): Promise<string> {
  try {
    return (await defaultAppManager.getDefaultApplication(type)).name;
  } catch (e) {
    if (NO_DEFAULT.includes((e as BusinessError).code)) {
      return '';
    }
    throw e as Error;
  }
}
