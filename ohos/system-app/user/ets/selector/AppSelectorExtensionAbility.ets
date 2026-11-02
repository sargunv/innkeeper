import { bundleManager, defaultAppManager, UIExtensionAbility, UIExtensionContentSession, Want } from '@kit.AbilityKit';
import dialogSession from '@ohos.app.ability.dialogSession';
import bundleResourceManager from '@ohos.bundle.bundleResourceManager';
import { BusinessError } from '@kit.BasicServicesKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { Choice, Choices, Picker } from '../chooser/Choice';
import { defaultType } from './Defaults';
import { SelectorDefaults } from './SelectorDefaults';

const DOMAIN = 0x0000;
const TAG = 'InnkeeperSelector';
const SESSION_ID = 'dialogSessionId';
const CLONE_INDEX = 'ohos.extra.param.key.appCloneIndex';

function text(v: Object | undefined): string {
  return typeof v === 'string' ? v : '';
}

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code !== undefined ? `${b.code} ${b.message}` : `${e}`;
}

// OpenHarmony's app selector, which AMS shows when several apps take an
// implicit start, such as an app's open of a file. AMS opens the selector of
// the app store that const.appgallery.shaderowner.bundlename names, as an
// AppSelectorExtensionAbility in a modal UIExtension, and
// sys_prod/etc/param/innkeeper_selector.para names Innkeeper User. The image's
// own selector, com.ohos.amsdialog, can't remember a choice on a phone.
//
// The selector reads AMS's dialog session for the apps and the want, and it
// shows them in the chooser's page. With "always", it makes the chosen app the
// user's default for the file's type, so the bundle manager hands later opens
// of that type straight to the app, and it records the default
// (SelectorDefaults.ets). It answers AMS with the chosen app, or with a
// refusal when the user cancels.
export default class AppSelectorExtensionAbility extends UIExtensionAbility {
  private pickers: Map<UIExtensionContentSession, SessionPicker> = new Map();

  onSessionCreate(want: Want, session: UIExtensionContentSession): void {
    const id = text(want.parameters?.[SESSION_ID]);
    const choices = new Choices();
    let type = '';
    try {
      const info = dialogSession.getDialogSessionInfo(id);
      const p: Record<string, Object> = info.parameters ?? {};
      type = defaultType(text(p['action']), text(p['wantType']), text(p['uri']));
      choices.apps = info.targetAbilityInfos.map((a: dialogSession.DialogAbilityInfo) => choice(a));
      choices.always = type !== '';
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'no dialog session: %{public}s', message(e as Object));
    }
    if (choices.apps.length === 0) {
      choices.error = 'No apps to choose from';
    }
    const picker = new SessionPicker(id, type, session, new SelectorDefaults(this.context));
    this.pickers.set(session, picker);
    const storage = new LocalStorage();
    storage.setOrCreate<Choices>('choices', choices);
    storage.setOrCreate<Picker>('picker', picker);
    session.loadContent('chooser/Chooser', storage);
    session.setWindowBackgroundColor('#00000000');
  }

  onSessionDestroy(session: UIExtensionContentSession): void {
    this.pickers.get(session)?.refuse();
    this.pickers.delete(session);
  }
}

// An app of the session, with its ability's label and its bundle's icon.
function choice(a: dialogSession.DialogAbilityInfo): Choice {
  const c = new Choice();
  c.target = { bundleName: a.bundleName, moduleName: a.moduleName, abilityName: a.abilityName };
  c.appIndex = a.appIndex;
  try {
    const r = bundleResourceManager.getBundleResourceInfo(a.bundleName,
      bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_LABEL |
        bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_ICON, a.appIndex);
    c.label = r.label;
    c.icon = r.icon;
  } catch (e) {
    hilog.warn(DOMAIN, TAG, 'no resources for %{public}s', a.bundleName);
  }
  try {
    c.label = bundleManager.getAbilityLabelSync(a.bundleName, a.moduleName, a.abilityName);
  } catch (e) {
    hilog.warn(DOMAIN, TAG, 'no label for %{public}s/%{public}s', a.bundleName, a.abilityName);
  }
  if (c.label === '') {
    c.label = a.bundleName;
  }
  return c;
}

// Answers AMS's dialog session once, with the chosen app or with a refusal.
class SessionPicker implements Picker {
  private id: string;
  private type: string;
  private session: UIExtensionContentSession;
  private defaults: SelectorDefaults;
  private answered: boolean = false;

  constructor(id: string, type: string, session: UIExtensionContentSession, defaults: SelectorDefaults) {
    this.id = id;
    this.type = type;
    this.session = session;
    this.defaults = defaults;
  }

  async pick(c: Choice, always: boolean): Promise<void> {
    if (always && this.type !== '') {
      try {
        await defaultAppManager.setDefaultApplication(this.type, {
          bundleName: c.target.bundleName,
          moduleName: c.target.moduleName,
          abilityName: c.target.abilityName,
        });
        hilog.info(DOMAIN, TAG, '%{public}s opens %{public}s by default', c.target.bundleName, this.type);
        await this.defaults.remember(this.type, c.target.bundleName);
      } catch (e) {
        hilog.warn(DOMAIN, TAG, 'not the default for %{public}s: %{public}s', this.type, message(e as Object));
      }
    }
    const parameters: Record<string, Object> = {};
    parameters[CLONE_INDEX] = c.appIndex;
    try {
      await dialogSession.sendDialogResult(this.id, {
        bundleName: c.target.bundleName,
        moduleName: c.target.moduleName,
        abilityName: c.target.abilityName,
        parameters: parameters,
      }, true);
    } catch (e) {
      throw new Error(`${c.label} didn't open (${message(e as Object)})`);
    }
    this.answered = true;
    hilog.info(DOMAIN, TAG, 'chose %{public}s', c.target.bundleName);
    this.session.terminateSelf();
  }

  close(): void {
    this.refuse();
    this.session.terminateSelf();
  }

  refuse(): void {
    if (this.answered) {
      return;
    }
    this.answered = true;
    dialogSession.sendDialogResult(this.id, {}, false).catch((e: Object) => {
      hilog.warn(DOMAIN, TAG, 'refusal not sent: %{public}s', message(e));
    });
  }
}
