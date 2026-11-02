import bundleManager from '@ohos.bundle.bundleManager';
import bundleMonitor from '@ohos.bundle.bundleMonitor';
import launcherBundleManager from '@ohos.bundle.launcherBundleManager';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, Reply, request } from 'libinnkeeper.so';
import { Message } from './Wire';

// host/bridge/src/apps/mod.rs
const HAVE = 0x0501;
const ADDED = 0x0502;
const REMOVED = 0x0503;
const MISSING = 0;
// The profile's user.
const USER = 100;
const SELF = 'dev.sargunv.innkeeper.system';
const DOMAIN = 0x0000;
const TAG = 'InnkeeperApps';

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

// Gives each launchable app that the user installed a desktop entry, as
// Android's AppMirrorFeature does. The bridge's apps service writes the
// entries. It reads the names, icon and URL schemes from the app's HAP, so
// the app sends an empty label and no icons. Preinstalled apps belong to the
// image and get no entry.
export class AppMirror {
  private apps: number;
  private onRemoved: () => void;
  // Requests go one at a time, in the order of the bundle events.
  private queue: Promise<void> = Promise.resolve();

  // `onRemoved` runs after each removal.
  constructor(apps: number, onRemoved: () => void) {
    this.apps = apps;
    this.onRemoved = onRemoved;
  }

  // Follows installs, updates and removals.
  watch(): void {
    const sync = (info: bundleMonitor.BundleChangedInfo): void => {
      if (info.userId === USER) {
        this.enqueue(() => this.sync(info.bundleName));
      }
    };
    try {
      bundleMonitor.on('add', sync);
      bundleMonitor.on('update', sync);
      bundleMonitor.on('remove', (info: bundleMonitor.BundleChangedInfo) => {
        if (info.userId === USER) {
          this.enqueue(() => this.removed(info.bundleName));
          this.onRemoved();
        }
      });
    } catch (e) {
      hilog.error(DOMAIN, TAG, 'bundle events: %{public}s', (e as Error).message);
    }
  }

  // Checks every launchable app, so that changes made while the bridge was
  // away catch up.
  syncAll(): void {
    this.enqueue(async (): Promise<void> => {
      const abilities = await launcherBundleManager.getAllLauncherAbilityInfo(USER);
      const seen = new Set<string>();
      for (const a of abilities) {
        const bundle = a.elementName.bundleName;
        if (!seen.has(bundle)) {
          seen.add(bundle);
          await this.sync(bundle);
        }
      }
    });
  }

  private enqueue(job: () => Promise<void>): void {
    this.queue = this.queue.then(job).catch((e: Error) => {
      hilog.error(DOMAIN, TAG, 'app mirror: %{public}s', e.message);
    });
  }

  private async sync(bundle: string): Promise<void> {
    if (bundle === SELF) {
      return;
    }
    let launchers: launcherBundleManager.LauncherAbilityInfo[] = [];
    try {
      launchers = await launcherBundleManager.getLauncherAbilityInfo(bundle, USER);
    } catch (e) {
      // The bundle has no launcher ability.
      return;
    }
    if (launchers.length === 0) {
      return;
    }
    const app = launchers[0].applicationInfo;
    if (app.systemApp || app.installSource === 'pre-installed') {
      return;
    }
    const info = await bundleManager.getBundleInfo(bundle,
      bundleManager.BundleFlag.GET_BUNDLE_INFO_DEFAULT, USER);
    // BMS keeps each module as `<module>.hap` in the bundle's code directory.
    const hap = `${app.codePath}/${launchers[0].elementName.moduleName}.hap`;
    const version = info.versionName;
    const have = await request(this.apps, HAVE, new Message().name(bundle).name(version).buffer(), []);
    close(have);
    if (have.status !== 0 || have.body.byteLength < 1 || new Uint8Array(have.body)[0] !== MISSING) {
      return;
    }
    const body = new Message()
      .name(bundle)
      .name(version)
      .name('') // label
      .u32(-1) // category
      .u8(0) // game
      .u32(0) // splash
      .u16(0)
      .u16(0) // window
      .name(hap)
      .u32(0) // icons
      .buffer();
    const r = await request(this.apps, ADDED, body, [blob(new ArrayBuffer(0))]);
    close(r);
    hilog.info(DOMAIN, TAG, '%{public}s %{public}s: desktop entry %{public}s', bundle, version,
      r.status === 0 ? 'written' : `failed (${r.status})`);
  }

  private async removed(bundle: string): Promise<void> {
    const r = await request(this.apps, REMOVED, new Message().name(bundle).buffer(), []);
    close(r);
    hilog.info(DOMAIN, TAG, '%{public}s: removed (%{public}d)', bundle, r.status);
  }
}
