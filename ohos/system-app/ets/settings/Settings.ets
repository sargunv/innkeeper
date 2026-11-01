import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { onEvent, open, Reply, request, waitParameter } from 'libinnkeeper.so';
import { decodeSettings, DesktopSettings } from './Desktop';
import { HostSettings } from './HostSettings';

// host/bridge/src/ipc/proto.rs and host/bridge/src/system/proto.rs
const SYSTEM = 3;
const START = 0x0301;
const SETTINGS = 0x4301;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperSettings';

// Connects to the bridge's system service once boot completes, and applies
// each SETTINGS event that it sends. START subscribes the connection, and the
// bridge then sends the current settings and each change. The connection
// also receives the clipboard's events, which it ignores.
export function startSettings(): void {
  const host = new HostSettings();
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    const system = open(SYSTEM, 'innkeeper-settings');
    onEvent(system, (type: number, body: ArrayBuffer, fds: number[]) => {
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === CONNECTED) {
        host.reset();
        request(system, START, new ArrayBuffer(0), []).then((reply: Reply) => {
          reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
        }).catch((e: Error) => {
          // The next connection starts again.
          hilog.info(DOMAIN, TAG, 'START not sent: %{public}s', e.message);
        });
      } else if (type === SETTINGS) {
        const s: DesktopSettings | undefined = decodeSettings(body);
        if (s === undefined) {
          hilog.warn(DOMAIN, TAG, 'malformed SETTINGS event');
          return;
        }
        hilog.info(DOMAIN, TAG, 'desktop settings: %{public}s', JSON.stringify(s));
        host.apply(s);
      }
    });
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
