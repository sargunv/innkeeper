import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import bundleResourceManager from '@ohos.bundle.bundleResourceManager';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { CHOOSE_TARGETS, CHOOSE_TYPES, CHOOSE_URIS } from '../files/Docs';
import { Target } from '../files/Targets';
import { Choice, Choices } from './Choice';

const DOMAIN = 0x0000;
const TAG = 'InnkeeperFiles';

function strings(v: Object | undefined): string[] {
  return Array.isArray(v) ? (v as Object[]).filter((s: Object) => typeof s === 'string') as string[] : [];
}

// The app chooser for files from the desktop, as Android's system app has
// one. OpenHarmony's own chooser needs an app store, so Innkeeper User shows
// this one when several apps open a batch of files
// (files/HostFiles.ets). The page lists the apps with their icons and labels,
// and it starts the app that the user picks with a read grant on the files.
export default class ChooserAbility extends UIAbility {
  private storage: LocalStorage = new LocalStorage();

  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.read(want);
  }

  onNewWant(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.read(want);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    windowStage.loadContent('chooser/Chooser', this.storage).then(() => {
      // The page draws its own backdrop over the window below. The window
      // takes a colour once it has content.
      windowStage.getMainWindowSync().setWindowBackgroundColor('#00000000');
    }).catch((e: Error) => {
      hilog.warn(DOMAIN, TAG, 'chooser not shown: %{public}s', e.message);
    });
  }

  private read(want: Want): void {
    const c = new Choices();
    c.uris = strings(want.parameters?.[CHOOSE_URIS]);
    c.types = strings(want.parameters?.[CHOOSE_TYPES]);
    let targets: Target[] = [];
    try {
      targets = JSON.parse(`${want.parameters?.[CHOOSE_TARGETS] ?? '[]'}`) as Target[];
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'malformed chooser targets');
    }
    c.apps = targets.map((t: Target) => {
      const a = new Choice();
      a.target = t;
      a.label = t.bundleName;
      try {
        const r = bundleResourceManager.getBundleResourceInfo(t.bundleName,
          bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_LABEL |
            bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_ICON);
        a.label = r.label;
        a.icon = r.icon;
      } catch (e) {
        hilog.warn(DOMAIN, TAG, 'no label for %{public}s', t.bundleName);
      }
      return a;
    }).sort((a: Choice, b: Choice) => a.label.toLowerCase().localeCompare(b.label.toLowerCase()));
    c.error = c.uris.length === 0 || c.apps.length === 0 ? 'No apps to choose from' : '';
    this.storage.setOrCreate<Choices>('choices', c);
  }
}
