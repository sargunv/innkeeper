import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import wantConstant from '@ohos.app.ability.wantConstant';
import { BusinessError } from '@ohos.base';
import uniformTypeDescriptor from '@ohos.data.uniformTypeDescriptor';
import fileUri from '@ohos.file.fileuri';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { onEvent, open, Reply, request } from 'libinnkeeper.so';
import { extension, OPEN, openBody, openWant, STAT, statBody, statName } from './Docs';

// host/bridge/src/ipc/proto.rs
const DOCUMENTS = 4;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperFiles';
// How long a launch waits for the documents connection.
const CONNECT_MS = 10000;

const decoder = util.TextDecoder.create('utf-8', { fatal: true, ignoreBOM: true });

function decode(b: Uint8Array): string | undefined {
  try {
    return decoder.decodeToString(b);
  } catch (e) {
    return undefined;
  }
}

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

// The MIME type of a file name's extension, or '' if OpenHarmony knows none.
function mimeOf(name: string): string {
  const ext = extension(name);
  if (ext === '') {
    return '';
  }
  try {
    const id = uniformTypeDescriptor.getUniformDataTypeByFilenameExtension(ext);
    const types = uniformTypeDescriptor.getTypeDescriptor(id).mimeTypes;
    return types.length > 0 ? types[0] : '';
  } catch (e) {
    return '';
  }
}

// Opens files from the desktop in OpenHarmony apps. `innkeeper run <bundle>
// <file>` and `innkeeper open` make a documents grant for each file, and
// Innkeeper System hands the grants to this service. The app copies each file
// into its own files, because OpenHarmony apps open files by path, and it
// starts the app with a read grant on the copies' URIs. OpenHarmony's URI
// permission service then shares the copies with that app. The copies stay
// until the next boot.
export class HostFiles {
  private context: common.ServiceExtensionContext;
  private docs: number = -1;
  private connected: boolean = false;
  private waiting: (() => void)[] = [];
  private copies: number = 0;
  private root: string;

  constructor(context: common.ServiceExtensionContext) {
    this.context = context;
    this.root = `${context.filesDir}/open`;
    try {
      fileIo.rmdirSync(this.root);
    } catch (e) {
      // No copies are left from an earlier boot.
    }
  }

  // Opens the documents connection, which stays open.
  start(): void {
    this.docs = open(DOCUMENTS, 'innkeeper-files');
    onEvent(this.docs, (type: number, body: ArrayBuffer, fds: number[]) => {
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === CONNECTED) {
        this.connected = true;
        this.waiting.splice(0).forEach((f: () => void) => f());
      }
    });
  }

  private ready(): Promise<void> {
    if (this.connected) {
      return Promise.resolve();
    }
    return new Promise<void>((resolve, reject) => {
      this.waiting.push(resolve);
      setTimeout(() => reject(new Error('the documents service is unavailable')), CONNECT_MS);
    });
  }

  // Copies the files of `grants` and opens them in `bundle`, or in the app
  // that OpenHarmony picks when `bundle` is empty.
  async open(bundle: string, grants: string[]): Promise<void> {
    try {
      await this.ready();
      const uris: string[] = [];
      let type = '';
      for (const g of grants) {
        // Each copy gets its own folder, so two files may have one name.
        const dir = `${this.root}/${++this.copies}`;
        fileIo.mkdirSync(dir, true);
        const path = await this.copy(g, dir);
        uris.push(fileUri.getUriFromPath(path));
        if (type === '') {
          type = mimeOf(path);
        }
      }
      const w = openWant(bundle, uris, type, wantConstant.Flags.FLAG_AUTH_READ_URI_PERMISSION);
      await this.context.startAbility(w as Want);
      hilog.info(DOMAIN, TAG, 'opened %{public}d files in %{public}s', uris.length,
        bundle === '' ? 'the chosen app' : bundle);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'opening files in %{public}s: %{public}s', bundle, message(e as Object));
    }
  }

  // Copies the file of grant `g` into `dir` and returns the copy's path.
  private async copy(g: string, dir: string): Promise<string> {
    const stat = await request(this.docs, STAT, statBody(g), []);
    close(stat);
    const name = stat.status === 0 ? statName(stat.body, decode) : undefined;
    if (name === undefined) {
      throw new Error(`grant ${g} isn't a file (${stat.status})`);
    }
    const opened = await request(this.docs, OPEN, openBody(g), []);
    if (opened.status !== 0 || opened.fds.length !== 1) {
      close(opened);
      throw new Error(`grant ${g} doesn't open (${opened.status})`);
    }
    const path = `${dir}/${name}`;
    try {
      fileIo.copyFileSync(opened.fds[0], path);
    } finally {
      close(opened);
    }
    return path;
  }
}
