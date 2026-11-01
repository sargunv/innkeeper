import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';

// The tests read the URI of each want from hilog.
function logUri(want: Want): void {
  if (want.uri) {
    console.info(`innkeeper-probes: opened ${want.uri}`);
  }
}

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    logUri(want);
  }

  onNewWant(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    logUri(want);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      windowStage.loadContent('pages/Index');
    } catch (e) {
      console.error(`loadContent failed: ${JSON.stringify(e)}`);
    }
  }
}
