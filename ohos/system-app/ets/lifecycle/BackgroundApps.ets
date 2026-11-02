import bundleManager from '@ohos.bundle.bundleManager';
import bundleResourceManager from '@ohos.bundle.bundleResourceManager';
import contextConstant from '@ohos.app.ability.contextConstant';
import preferences from '@ohos.data.preferences';
import common from '@ohos.app.ability.common';
import { BackgroundApp } from './Report';
import { withBundle } from './Background';

// The profile's user.
const USER = 100;
const KEY = 'background';

// The apps that the user lets run in the background, which is off by
// default, as Android's BackgroundApps keeps them. While one is installed,
// the session doesn't stop when idle, and the desktop shows an indicator.
// Session1's `background` app action sets it
// (`innkeeper app set BUNDLE --background on`). The set lives in EL1
// storage, which is readable before the user unlocks.
export class BackgroundApps {
  // Called after each change.
  onChange: () => void = () => {};
  private prefs: preferences.Preferences;

  constructor(context: common.Context) {
    this.prefs = preferences.getPreferencesSync(
      context.createAreaModeContext(contextConstant.AreaMode.EL1), { name: 'lifecycle' });
  }

  // The allowed apps that are still installed, sorted, with their labels.
  async allowed(): Promise<BackgroundApp[]> {
    const apps: BackgroundApp[] = [];
    for (const bundle of this.stored()) {
      if (await installed(bundle)) {
        apps.push({ bundle: bundle, label: label(bundle) });
      }
    }
    return apps;
  }

  // Returns an error, or an empty string once set. Only installed user apps
  // can be changed.
  async set(bundle: string, enabled: boolean): Promise<string> {
    let info: bundleManager.ApplicationInfo;
    try {
      info = await bundleManager.getApplicationInfo(bundle, 0, USER);
    } catch (e) {
      return `unknown package ${bundle}`;
    }
    if (info.systemApp) {
      return 'Only installed user apps can be managed';
    }
    this.prefs.putSync(KEY, withBundle(this.stored(), bundle, enabled));
    await this.prefs.flush();
    this.onChange();
    return '';
  }

  private stored(): string[] {
    return this.prefs.getSync(KEY, [] as string[]) as string[];
  }
}

async function installed(bundle: string): Promise<boolean> {
  try {
    await bundleManager.getApplicationInfo(bundle, 0, USER);
    return true;
  } catch (e) {
    return false;
  }
}

function label(bundle: string): string {
  try {
    const l = bundleResourceManager.getBundleResourceInfo(bundle,
      bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_LABEL).label;
    return l.length > 0 ? l : bundle;
  } catch (e) {
    return bundle;
  }
}
