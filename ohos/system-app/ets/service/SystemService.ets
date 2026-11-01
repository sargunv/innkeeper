import ServiceExtensionAbility from '@ohos.app.ability.ServiceExtensionAbility';
import Want from '@ohos.app.ability.Want';
import { startApps } from '../bridge/Apps';
import { BackgroundApps } from '../lifecycle/BackgroundApps';
import { startLifecycle } from '../lifecycle/SessionLifecycle';
import { startSettings } from '../settings/Settings';

// The app's main element. The profile's layer marks the app keep-alive
// (install_list_capability.json), so OpenHarmony starts this ability at boot
// and restarts it if its process dies.
export default class SystemService extends ServiceExtensionAbility {
  onCreate(want: Want): void {
    const background = new BackgroundApps(this.context);
    startApps(background);
    startLifecycle(background);
    startSettings();
  }
}
