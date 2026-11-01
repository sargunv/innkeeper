import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { onEvent, open, request, waitParameter } from 'libinnkeeper.so';

// host/bridge/src/ipc/proto.rs and host/bridge/src/session.rs
const APPS = 5;
const BOOT_COMPLETED = 0x0505;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperSystem';

// Reports boot completion on the apps channel. The bridge's Session1 counts
// the session as booted once the current apps connection has sent
// BOOT_COMPLETED, so the app sends it after `bootevent.boot.completed` and
// again on each new connection.
export function reportBootCompletion(): void {
  const apps = open(APPS, 'innkeeper-system');
  let booted = false;
  const report = (): void => {
    request(apps, BOOT_COMPLETED, new ArrayBuffer(0), []).then((reply) => {
      if (reply.status !== 0) {
        hilog.error(DOMAIN, TAG, 'BOOT_COMPLETED failed: %{public}d', reply.status);
      }
    }).catch((e: Error) => {
      // The next connection reports it.
      hilog.info(DOMAIN, TAG, 'BOOT_COMPLETED not sent: %{public}s', e.message);
    });
  };
  onEvent(apps, (type: number, body: ArrayBuffer, fds: number[]) => {
    // The app handles no apps events yet.
    fds.forEach((fd: number) => fileIo.closeSync(fd));
    if (type === CONNECTED && booted) {
      report();
    }
  });
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    hilog.info(DOMAIN, TAG, 'boot completed');
    booted = true;
    report();
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
