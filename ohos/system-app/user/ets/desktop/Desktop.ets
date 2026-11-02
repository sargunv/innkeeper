import common from '@ohos.app.ability.common';
import { BusinessError } from '@ohos.base';
import defaultAppManager from '@ohos.bundle.defaultAppManager';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, onEvent, open, Reply, request } from 'libinnkeeper.so';
import { HANDOFF, handoffBody, handoffMetadata, isLink } from './Handoff';

// host/bridge/src/ipc/proto.rs
const SYSTEM = 3;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperDesktop';
const CONNECT_MS = 10000;
// Marks that the app made itself the default browser once, so a later
// choice in OpenHarmony's Settings stays.
const BROWSER_SET = 'default-browser-set';

const encoder = new util.TextEncoder();

function encode(s: string): Uint8Array {
  // OpenHarmony's encoder returns nothing for an empty string.
  return s.length === 0 ? new Uint8Array(0) : encoder.encodeInto(s);
}

export function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

let system = -1;
let connected = false;
const waiting: (() => void)[] = [];

// The connection to the bridge's system service, which the process opens
// once. HANDOFF needs no START.
function connection(): Promise<number> {
  if (system < 0) {
    system = open(SYSTEM, 'innkeeper-desktop');
    onEvent(system, (type: number, body: ArrayBuffer, fds: number[]) => {
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === CONNECTED) {
        connected = true;
        waiting.splice(0).forEach((f: () => void) => f());
      }
    });
  }
  if (connected) {
    return Promise.resolve(system);
  }
  return new Promise<number>((resolve, reject) => {
    waiting.push(() => resolve(system));
    setTimeout(() => reject(new Error('the bridge is unavailable')), CONNECT_MS);
  });
}

// Opens the want's link on the desktop, in the desktop's handler for its
// scheme.
export async function toDesktop(uri: string): Promise<void> {
  if (!isLink(uri)) {
    hilog.warn(DOMAIN, TAG, 'not a link for the desktop: %{private}s', uri);
    return;
  }
  const c = await connection();
  const meta = handoffMetadata(encode(uri));
  const reply: Reply = await request(c, HANDOFF, handoffBody(meta.length), [blob(meta.buffer)]);
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
  if (reply.status !== 0) {
    throw new Error(`the bridge refused the link (${reply.status})`);
  }
  hilog.info(DOMAIN, TAG, 'opened a link on the desktop');
}

// Makes DesktopAbility the default browser of `user` once, as Android's
// system app holds the browser role. Apps' links then open on the desktop.
export function setDefaultBrowser(context: common.ServiceExtensionContext, user: number): void {
  const mark = `${context.filesDir}/${BROWSER_SET}`;
  if (fileIo.accessSync(mark)) {
    return;
  }
  defaultAppManager.setDefaultApplication(defaultAppManager.ApplicationType.BROWSER, {
    bundleName: context.applicationInfo.name,
    moduleName: 'entry',
    abilityName: 'DesktopAbility',
  }, user).then(() => {
    fileIo.closeSync(fileIo.openSync(mark, fileIo.OpenMode.CREATE));
    hilog.info(DOMAIN, TAG, 'the desktop is the default browser');
  }).catch((e: Object) => {
    hilog.warn(DOMAIN, TAG, 'not the default browser: %{public}s', message(e));
  });
}

