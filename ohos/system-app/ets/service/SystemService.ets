import ServiceExtensionAbility from '@ohos.app.ability.ServiceExtensionAbility';
import Want from '@ohos.app.ability.Want';
import { startApps } from '../bridge/Apps';

// The app's main element. The profile's layer marks the app keep-alive
// (install_list_capability.json), so OpenHarmony starts this ability at boot
// and restarts it if its process dies.
export default class SystemService extends ServiceExtensionAbility {
  onCreate(want: Want): void {
    startApps();
  }
}
