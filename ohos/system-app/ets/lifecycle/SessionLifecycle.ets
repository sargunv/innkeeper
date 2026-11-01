import backgroundTaskManager from '@ohos.resourceschedule.backgroundTaskManager';
import bundleManager from '@ohos.bundle.bundleManager';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, onEvent, open, Reply, request, waitParameter } from 'libinnkeeper.so';
import { utf8 } from '../bridge/Wire';
import { BackgroundApps } from './BackgroundApps';
import { backgroundText, body, LIFECYCLE, services, Task } from './Report';

// host/bridge/src/ipc/proto.rs
const SYSTEM = 3;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
// The profile's user.
const USER = 100;
// Task events poke the report. The poll catches what they miss.
const POLL_MS = 15000;
const DEBOUNCE_MS = 200;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperLifecycle';

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

// Reports what keeps the session up to the bridge's system service, as
// Android's SessionLifecycle does, whenever it changes. The launcher stops
// the session after a grace period with nothing to do.
class SessionLifecycle {
  private system: number = open(SYSTEM, 'innkeeper-lifecycle');
  private background: BackgroundApps;
  // The report that the bridge has, or an empty string if it has none.
  private sent: string = '';
  private timer: number = -1;
  private queue: Promise<void> = Promise.resolve();
  // Whether each bundle seen is a user app. That can't change while the
  // bundle stays installed.
  private userApps: Map<string, boolean> = new Map();

  constructor(background: BackgroundApps) {
    this.background = background;
  }

  start(): void {
    this.background.onChange = (): void => this.poke();
    onEvent(this.system, (type: number, _body: ArrayBuffer, fds: number[]) => {
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === CONNECTED) {
        this.sent = '';
        this.poke();
      }
    });
    const subscriber: backgroundTaskManager.BackgroundTaskSubscriber = {
      onContinuousTaskStart: (): void => this.poke(),
      onContinuousTaskUpdate: (): void => this.poke(),
      onContinuousTaskStop: (): void => this.poke(),
    };
    try {
      backgroundTaskManager.subscribeContinuousTaskState(subscriber);
    } catch (e) {
      hilog.error(DOMAIN, TAG, 'continuous task events: %{public}s', (e as Error).message);
    }
    setInterval(() => this.poke(), POLL_MS);
    this.poke();
  }

  // Reports soon, because something may have changed.
  poke(): void {
    clearTimeout(this.timer);
    this.timer = setTimeout(() => {
      this.queue = this.queue.then(() => this.update()).catch((e: Error) => {
        hilog.error(DOMAIN, TAG, 'lifecycle: %{public}s', e.message);
      });
    }, DEBOUNCE_MS);
  }

  private async update(): Promise<void> {
    const count = await this.services();
    const text = backgroundText(await this.background.allowed());
    const report = `services=${count} background=${text}`;
    if (report === this.sent) {
      return;
    }
    const bytes = utf8(text);
    const fds = bytes.length > 0 ? [blob(new Uint8Array(bytes).buffer)] : [];
    const reply = await request(this.system, LIFECYCLE, body(count, bytes.length), fds);
    close(reply);
    if (reply.status !== 0) {
      hilog.error(DOMAIN, TAG, 'LIFECYCLE refused: %{public}d', reply.status);
      return;
    }
    hilog.info(DOMAIN, TAG, 'lifecycle: %{public}s', JSON.stringify(report));
    this.sent = report;
  }

  private async services(): Promise<number> {
    const all = await backgroundTaskManager.obtainAllContinuousTasks();
    const tasks: Task[] = [];
    const users = new Set<string>();
    for (const t of all) {
      const bundle = t.bundleName ?? await bundleManager.getBundleNameByUid(t.uid);
      tasks.push({ bundle: bundle, suspended: t.suspendState });
      if (await this.isUserApp(bundle)) {
        users.add(bundle);
      }
    }
    return services(tasks, users);
  }

  private async isUserApp(bundle: string): Promise<boolean> {
    let user = this.userApps.get(bundle);
    if (user === undefined) {
      try {
        user = !(await bundleManager.getApplicationInfo(bundle, 0, USER)).systemApp;
      } catch (e) {
        // A bundle that is gone runs nothing.
        return false;
      }
      this.userApps.set(bundle, user);
    }
    return user;
  }
}

// Starts the reports once boot completes. The screen window is open by then,
// so an early report doesn't stop a session that is still coming up.
export function startLifecycle(background: BackgroundApps): void {
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    new SessionLifecycle(background).start();
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
