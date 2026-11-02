import appManager from '@ohos.app.ability.appManager';
import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import osAccount from '@ohos.account.osAccount';
import { BusinessError } from '@ohos.base';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { waitParameter } from 'libinnkeeper.so';
import { Answer } from './Answer';

const BUNDLE = 'dev.sargunv.innkeeper.user';
// An app's uid is its user's id times this, plus the app's id.
const UID_PER_USER = 200000;
// A process that keeps dying restarts at most once a second.
const RESTART_MS = 1000;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperSystem';
// How long a request waits for Innkeeper User's answer. The bridge's caller
// waits 40 s.
const ANSWER_MS = 30000;

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

// The requests that wait for Innkeeper User's answer.
const pending = new Map<number, (a: Answer) => void>();
let requests = 0;

function failed(error: string): Answer {
  const a = new Answer();
  a.error = error;
  return a;
}

// Starts Innkeeper User's service as the foreground user with the want
// parameters `p` and a request number. The service answers through
// answered().
async function askUser(context: common.ServiceExtensionContext, p: Record<string, Object>): Promise<Answer> {
  const request = ++requests;
  const answer = new Promise<Answer>((resolve) => {
    const timer = setTimeout(() => answered(request, failed("Innkeeper User didn't answer")), ANSWER_MS);
    pending.set(request, (a: Answer) => {
      clearTimeout(timer);
      pending.delete(request);
      resolve(a);
    });
  });
  try {
    const user = await osAccount.getAccountManager().getForegroundOsAccountLocalId();
    p['innkeeper.request'] = request;
    const want: Want = { bundleName: BUNDLE, abilityName: 'UserService', parameters: p };
    await context.startServiceExtensionAbilityWithAccount(want, user);
  } catch (e) {
    answered(request, failed(`Innkeeper User didn't start: ${message(e as Object)}`));
  }
  return answer;
}

// Hands files from the desktop to Innkeeper User, which runs as the
// foreground user and owns the copies that apps open
// (user/ets/files/HostFiles.ets).
export function openInUser(context: common.ServiceExtensionContext, bundle: string,
  grants: string[]): Promise<Answer> {
  const p: Record<string, Object> = {
    'innkeeper.open.bundle': bundle,
    'innkeeper.open.grants': grants.join(','),
  };
  return askUser(context, p);
}

// Asks Innkeeper User for the types that `bundle` opens by default, among
// those that its app selector set, after it resets them with `clear`
// (user/ets/selector/SelectorDefaults.ets). The reply is a JSON array.
export function defaultsInUser(context: common.ServiceExtensionContext, bundle: string,
  clear: boolean): Promise<Answer> {
  const p: Record<string, Object> = {
    'innkeeper.defaults.bundle': bundle,
    'innkeeper.defaults.clear': clear,
  };
  return askUser(context, p);
}

// Ends request `request` with Innkeeper User's answer `a`.
export function answered(request: number, a: Answer): void {
  const resolve = pending.get(request);
  if (resolve !== undefined) {
    resolve(a);
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
