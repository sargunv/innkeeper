import ServiceExtensionAbility from '@ohos.app.ability.ServiceExtensionAbility';
import Want from '@ohos.app.ability.Want';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { startClipboard } from '../clipboard/Clipboard';
import { HostFiles } from '../files/HostFiles';

const DOMAIN = 0x0000;
const TAG = 'InnkeeperUser';
// An app's uid is its user's id times this, plus the app's id.
const UID_PER_USER = 200000;

// The app's main element. Innkeeper System runs as user 0, and it starts this
// ability as the foreground user, so the ability's process runs as that
// user. The features whose OpenHarmony services keep a state for each user
// start here.
export default class UserService extends ServiceExtensionAbility {
  private files?: HostFiles;

  onCreate(want: Want): void {
    const user = Math.floor(this.context.applicationInfo.uid / UID_PER_USER);
    hilog.info(DOMAIN, TAG, 'started for user %{public}d', user);
    startClipboard(this.context);
    this.files = new HostFiles(this.context);
    this.files.start();
  }

  // Innkeeper System starts the service again with files from the desktop
  // (ets/service/PerUser.ets in Innkeeper System).
  onRequest(want: Want, startId: number): void {
    const bundle = want.parameters?.['innkeeper.open.bundle'];
    const grants = want.parameters?.['innkeeper.open.grants'];
    if (typeof bundle === 'string' && typeof grants === 'string' && grants !== '') {
      this.files?.open(bundle as string, (grants as string).split(','));
    }
  }
}
