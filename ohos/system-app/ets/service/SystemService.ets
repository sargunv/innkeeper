import ServiceExtensionAbility from '@ohos.app.ability.ServiceExtensionAbility';
import Want from '@ohos.app.ability.Want';
import { startApps } from '../bridge/Apps';
import { BackgroundApps } from '../lifecycle/BackgroundApps';
import { startLifecycle } from '../lifecycle/SessionLifecycle';
import { startMedia } from '../media/HostMedia';
import { startNotifications } from '../notify/Notifications';
import { userAnswer } from './Answer';
import { answered, startPerUser } from './PerUser';
import { startSettings } from '../settings/Settings';

// The app's main element. The profile's layer marks the app keep-alive
// (install_list_capability.json), so OpenHarmony starts this ability at boot
// and restarts it if its process dies.
export default class SystemService extends ServiceExtensionAbility {
  onCreate(want: Want): void {
    const background = new BackgroundApps(this.context);
    startApps(this.context, background);
    startLifecycle(background);
    startNotifications(this.context);
    startMedia(this.context);
    startSettings();
    startPerUser(this.context);
  }

  // Innkeeper User starts the service again with its answer to a request.
  onRequest(want: Want, startId: number): void {
    const a = userAnswer(want.parameters);
    if (a !== undefined) {
      answered(a.request, a);
    }
  }
}
