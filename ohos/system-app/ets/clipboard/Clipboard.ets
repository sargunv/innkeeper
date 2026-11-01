import common from '@ohos.app.ability.common';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { onEvent, open, Reply, request, waitParameter } from 'libinnkeeper.so';
import { ClipHeader, decodeHeader } from './Clip';
import { HostClipboard } from './HostClipboard';

// host/bridge/src/ipc/proto.rs and host/bridge/src/system/proto.rs
const SYSTEM = 3;
const START = 0x0301;
const CLIP = 0x4303;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperClipboard';

// Connects to the bridge's system service once boot completes and syncs the
// clipboard on that connection. START subscribes the connection to the
// desktop's clipboard, and the bridge then sends the current clip and each
// change. The connection also receives the desktop's settings, which it
// ignores.
export function startClipboard(context: common.Context): void {
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    const system = open(SYSTEM, 'innkeeper-clipboard');
    const clipboard = new HostClipboard(context, system);
    onEvent(system, (type: number, body: ArrayBuffer, fds: number[]) => {
      if (type === CLIP && fds.length === 1) {
        const h: ClipHeader | undefined = decodeHeader(body);
        if (h !== undefined) {
          clipboard.fromDesktop(h, fds[0]);
          return;
        }
        hilog.warn(DOMAIN, TAG, 'malformed CLIP event');
      }
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === CONNECTED) {
        request(system, START, new ArrayBuffer(0), []).then((reply: Reply) => {
          reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
        }).catch((e: Error) => {
          // The next connection starts again.
          hilog.info(DOMAIN, TAG, 'START not sent: %{public}s', e.message);
        });
      }
    });
    clipboard.watch();
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
