import appManager from '@ohos.app.ability.appManager';
import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import osAccount from '@ohos.account.osAccount';
import { BusinessError } from '@ohos.base';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { waitParameter } from 'libinnkeeper.so';

const BUNDLE = 'dev.sargunv.innkeeper.user';
// An app's uid is its user's id times this, plus the app's id.
const UID_PER_USER = 200000;
// A process that keeps dying restarts at most once a second.
const RESTART_MS = 1000;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperSystem';

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

// Runs Innkeeper User for the foreground user, and starts it again when its
// process dies. Innkeeper System is a singleton and runs as user 0.
// OpenHarmony keeps some state for each user, such as the pasteboard, and
// apps run as the foreground user. AMS keeps alive no app outside user 0, so
// Innkeeper System starts Innkeeper User's service as the foreground user.
export function startPerUser(context: common.ServiceExtensionContext): void {
  const want: Want = { bundleName: BUNDLE, abilityName: 'UserService' };
  // The foreground user is active once boot completes.
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    return osAccount.getAccountManager().getForegroundOsAccountLocalId();
  }).then((user: number) => {
    const start = (): void => {
      context.startServiceExtensionAbilityWithAccount(want, user).then(() => {
        hilog.info(DOMAIN, TAG, 'Innkeeper User runs for user %{public}d', user);
      }).catch((e: Object) => {
        hilog.error(DOMAIN, TAG, 'Innkeeper User not started: %{public}s', message(e));
      });
    };
    const observer: appManager.ApplicationStateObserver = {
      onProcessDied: (p: appManager.ProcessData): void => {
        if (Math.floor(p.uid / UID_PER_USER) === user) {
          hilog.warn(DOMAIN, TAG, 'Innkeeper User stopped, starting it again');
          setTimeout(start, RESTART_MS);
        }
      },
      onForegroundApplicationChanged: (): void => {},
      onAbilityStateChanged: (): void => {},
      onProcessCreated: (): void => {},
      onProcessStateChanged: (): void => {},
      onAppStarted: (): void => {},
      onAppStopped: (): void => {},
    };
    appManager.on('applicationState', observer, [BUNDLE]);
    start();
  }).catch((e: Object) => {
    hilog.error(DOMAIN, TAG, 'Innkeeper User not started: %{public}s', message(e));
  });
}
