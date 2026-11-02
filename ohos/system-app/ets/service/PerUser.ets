import appManager from '@ohos.app.ability.appManager';
import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import osAccount from '@ohos.account.osAccount';
import { BusinessError } from '@ohos.base';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { waitParameter } from 'libinnkeeper.so';
import { Opened } from '../open/Launch';

const BUNDLE = 'dev.sargunv.innkeeper.user';
// An app's uid is its user's id times this, plus the app's id.
const UID_PER_USER = 200000;
// A process that keeps dying restarts at most once a second.
const RESTART_MS = 1000;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperSystem';
// How long a file launch waits for Innkeeper User's answer. The bridge's
// caller waits 40 s.
const ANSWER_MS = 30000;

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

// The file launches that wait for Innkeeper User's answer, by request.
const pending = new Map<number, (o: Opened) => void>();
let requests = 0;

function failed(error: string): Opened {
  const o = new Opened();
  o.error = error;
  return o;
}

// Hands files from the desktop to Innkeeper User, which runs as the
// foreground user and owns the copies that apps open
// (user/ets/files/HostFiles.ets). The service gets the grants in its
// onRequest, and it answers through answered().
export async function openInUser(context: common.ServiceExtensionContext, bundle: string,
  grants: string[]): Promise<Opened> {
  const request = ++requests;
  const answer = new Promise<Opened>((resolve) => {
    const timer = setTimeout(() => answered(request, failed("Innkeeper User didn't answer")), ANSWER_MS);
    pending.set(request, (o: Opened) => {
      clearTimeout(timer);
      pending.delete(request);
      resolve(o);
    });
  });
  try {
    const user = await osAccount.getAccountManager().getForegroundOsAccountLocalId();
    const want: Want = {
      bundleName: BUNDLE,
      abilityName: 'UserService',
      parameters: {
        'innkeeper.open.bundle': bundle,
        'innkeeper.open.grants': grants.join(','),
        'innkeeper.open.request': request,
      },
    };
    await context.startServiceExtensionAbilityWithAccount(want, user);
  } catch (e) {
    answered(request, failed(`Innkeeper User didn't start: ${message(e as Object)}`));
  }
  return answer;
}

// Ends file launch `request` with Innkeeper User's answer `o`.
export function answered(request: number, o: Opened): void {
  const resolve = pending.get(request);
  if (resolve !== undefined) {
    resolve(o);
  }
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
