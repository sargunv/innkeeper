import { UIAbility } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';

export default class EntryAbility extends UIAbility {
  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      windowStage.loadContent('pages/Index');
    } catch (e) {
      console.error(`loadContent failed: ${JSON.stringify(e)}`);
    }
  }
}
