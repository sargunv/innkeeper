import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import wantConstant from '@ohos.app.ability.wantConstant';
import { BusinessError } from '@ohos.base';
import bundleManager from '@ohos.bundle.bundleManager';
import fileUri from '@ohos.file.fileuri';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { onEvent, open, Reply, request } from 'libinnkeeper.so';
import { CHOOSE_TARGETS, CHOOSE_TYPES, CHOOSE_URIS, OPEN, openBody, openWant, STAT, statBody, statName, targetWant } from './Docs';
import { mimeOf } from './Mime';
import { Ability, ABILITY_NOT_FOUND, distinctTypes, noTarget, openError, opened, Target, targets } from './Targets';

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

// The result of opening a batch of files, which goes back to the desktop.
export class Outcome {
  error: string = '';
  reply: string = '';
}

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
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
      setTimeout(() => reject(new Error("The desktop's files aren't available")), CONNECT_MS);
    });
  }

  // Copies the files of `grants` and opens them in `bundle`, or in the app
  // that the user picks when `bundle` is empty. An app that alone takes the
  // files opens at once, and several apps show the app chooser.
  async open(bundle: string, grants: string[]): Promise<Outcome> {
    const o = new Outcome();
    const types: string[] = [];
    let target = bundle;
    try {
      await this.ready();
      const uris: string[] = [];
      for (const g of grants) {
        // Each copy gets its own folder, so two files may have one name.
        const dir = `${this.root}/${++this.copies}`;
        fileIo.mkdirSync(dir, true);
        const path = await this.copy(g, dir);
        uris.push(fileUri.getUriFromPath(path));
        types.push(mimeOf(path));
      }
      const apps = await this.targets(bundle, uris, types);
      if (apps.length === 0) {
        throw new Error(noTarget(bundle, types));
      }
      if (apps.length === 1) {
        target = apps[0].bundleName;
        await this.context.startAbility(targetWant(apps[0], uris, types, wantConstant.Flags.FLAG_AUTH_READ_URI_PERMISSION) as Want);
      } else {
        target = '';
        const p: Record<string, Object> = {};
        p[CHOOSE_URIS] = uris;
        p[CHOOSE_TYPES] = types;
        p[CHOOSE_TARGETS] = JSON.stringify(apps);
        await this.context.startAbility({
          bundleName: this.context.applicationInfo.name,
          abilityName: 'ChooserAbility',
          parameters: p,
        });
      }
      o.reply = opened(uris.length, target);
      hilog.info(DOMAIN, TAG, '%{public}s', o.reply);
    } catch (e) {
      const b = e as BusinessError;
      o.error = openError(b.code, `${b.message}`, target, types);
      hilog.warn(DOMAIN, TAG, 'opening files in %{public}s: %{public}s', bundle, message(e as Object));
    }
    return o;
  }

  // The abilities that open every file of a batch, in `bundle` or in any app
  // when `bundle` is empty.
  private async targets(bundle: string, uris: string[], types: string[]): Promise<Target[]> {
    const matches: Ability[][] = [];
    for (const type of distinctTypes(types)) {
      const w = openWant(bundle, uris.slice(0, 1), type, 0) as Want;
      try {
        const found = await bundleManager.queryAbilityInfo(w, 0);
        matches.push(found.map((a: bundleManager.AbilityInfo): Ability => ({
          bundleName: a.bundleName,
          moduleName: a.moduleName,
          name: a.name,
          exported: a.exported,
        })));
      } catch (e) {
        // The bundle manager throws this when no ability takes the type.
        if ((e as BusinessError).code !== ABILITY_NOT_FOUND) {
          throw e as Error;
        }
        matches.push([]);
      }
    }
    return targets(matches, this.context.applicationInfo.name);
  }

  // Copies the file of grant `g` into `dir` and returns the copy's path.
  private async copy(g: string, dir: string): Promise<string> {
    const stat = await request(this.docs, STAT, statBody(g), []);
    close(stat);
    const name = stat.status === 0 ? statName(stat.body, decode) : undefined;
    if (name === undefined) {
      throw new Error(`A file from the desktop isn't available (${stat.status})`);
    }
    const file = await request(this.docs, OPEN, openBody(g), []);
    if (file.status !== 0 || file.fds.length !== 1) {
      close(file);
      throw new Error(`A file from the desktop doesn't open (${file.status})`);
    }
    const path = `${dir}/${name}`;
    try {
      fileIo.copyFileSync(file.fds[0], path);
    } finally {
      close(file);
    }
    return path;
  }
}
