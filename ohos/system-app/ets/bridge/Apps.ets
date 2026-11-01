import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, onEvent, open, Reply, request, waitParameter } from 'libinnkeeper.so';
import { AppMirror } from './AppMirror';
import { Message, utf8 } from './Wire';

// host/bridge/src/ipc/proto.rs and host/bridge/src/session.rs
const APPS = 5;
const BOOT_COMPLETED = 0x0505;
const COMMAND = 0x4501;
const RESULT = 0x0504;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperSystem';
// The answer to every Session1 command that the bridge relays. It fits both
// the app and the window responses (host/core/src/app_settings.rs).
const UNSUPPORTED = '{"error":"OpenHarmony profiles don\'t support this action.","info":null,"reply":null}';

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

// Answers a COMMAND event, whose body is `u64 id, u32 length`, with a RESULT
// in the same envelope. The bridge's caller waits up to 40 s for it.
function answer(apps: number, body: ArrayBuffer): void {
  if (body.byteLength !== 12) {
    return;
  }
  const json = utf8(UNSUPPORTED);
  const reply = new Message().raw(new Uint8Array(body, 0, 8)).u32(json.length).buffer();
  request(apps, RESULT, reply, [blob(new Uint8Array(json).buffer)]).then(close).catch((e: Error) => {
    hilog.info(DOMAIN, TAG, 'RESULT not sent: %{public}s', e.message);
  });
}

// Connects to the bridge's apps service. The bridge's Session1 counts the
// session as booted once the current apps connection has sent
// BOOT_COMPLETED, so the app sends it after `bootevent.boot.completed` and
// again on each new connection. From then on the app also mirrors the
// installed apps to the desktop's app menu.
export function startApps(): void {
  const apps = open(APPS, 'innkeeper-system');
  const mirror = new AppMirror(apps);
  let booted = false;
  const report = (): void => {
    request(apps, BOOT_COMPLETED, new ArrayBuffer(0), []).then((reply: Reply) => {
      close(reply);
      if (reply.status !== 0) {
        hilog.error(DOMAIN, TAG, 'BOOT_COMPLETED failed: %{public}d', reply.status);
      }
    }).catch((e: Error) => {
      // The next connection reports it.
      hilog.info(DOMAIN, TAG, 'BOOT_COMPLETED not sent: %{public}s', e.message);
    });
    mirror.syncAll();
  };
  onEvent(apps, (type: number, body: ArrayBuffer, fds: number[]) => {
    fds.forEach((fd: number) => fileIo.closeSync(fd));
    if (type === COMMAND) {
      answer(apps, body);
    } else if (type === CONNECTED && booted) {
      report();
    }
  });
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    hilog.info(DOMAIN, TAG, 'boot completed');
    booted = true;
    mirror.watch();
    report();
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
