import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { message, toDesktop } from './Desktop';

// Opens links from OpenHarmony apps on the desktop, as Android's
// HandoffActivity does. It is the default browser. It shows no page and
// finishes once the bridge has the link.
export default class DesktopAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.handle(want);
  }

  onNewWant(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    this.handle(want);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
  }

  private handle(want: Want): void {
    toDesktop(want.uri ?? '').catch((e: Object) => {
      hilog.warn(0x0000, 'InnkeeperDesktop', 'handoff failed: %{public}s', message(e));
    }).finally(() => {
      this.context.terminateSelf();
    });
  }
}
