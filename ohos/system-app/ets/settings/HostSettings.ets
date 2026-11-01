import config from '@ohos.accessibility.config';
import i18n from '@ohos.i18n';
import systemDateTime from '@ohos.systemDateTime';
import uiAppearance from '@ohos.uiAppearance';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { DesktopSettings, Plan, plan, weekDayOf } from './Desktop';

const DOMAIN = 0x0000;
const TAG = 'InnkeeperSettings';
const WEEK_DAYS: i18n.WeekDay[] = [
  i18n.WeekDay.MON, i18n.WeekDay.TUE, i18n.WeekDay.WED, i18n.WeekDay.THU,
  i18n.WeekDay.FRI, i18n.WeekDay.SAT, i18n.WeekDay.SUN,
];

function log(what: string): void {
  hilog.info(DOMAIN, TAG, '%{public}s', what);
}

function failed(what: string, e: Error): void {
  hilog.warn(DOMAIN, TAG, '%{public}s: %{public}s', what, e.message);
}

// Runs one change, so that a failure leaves the others to run.
function step(what: string, f: () => void): void {
  try {
    f();
  } catch (e) {
    failed(what, e as Error);
  }
}

// Applies the desktop's settings to OpenHarmony, as Android's HostSettings.kt
// does. Each change except the clock skips a value that OpenHarmony already
// has.
export class HostSettings {
  private last: DesktopSettings | undefined = undefined;

  // Forgets what was applied, so that the next event applies everything.
  reset(): void {
    this.last = undefined;
  }

  apply(s: DesktopSettings): void {
    const p = plan(this.last, s);
    this.last = s;
    step('dark mode', () => this.darkMode(p));
    step('font scale', () => this.fontScale(p));
    step('accessibility', () => this.accessibility(p));
    step('time zone', () => this.timezone(p));
    step('languages', () => this.languages(p));
    step('formats', () => this.formats(p));
    // A locale's clock is OpenHarmony's default, so the clock applies after
    // the locale.
    step('clock', () => this.clock(p));
  }

  private darkMode(p: Plan): void {
    if (p.darkMode < 0) {
      return;
    }
    const mode = p.darkMode === 0 ? uiAppearance.DarkMode.ALWAYS_DARK : uiAppearance.DarkMode.ALWAYS_LIGHT;
    if (uiAppearance.getDarkMode() === mode) {
      return;
    }
    uiAppearance.setDarkMode(mode).then(() => {
      log(`dark mode -> ${p.darkMode === 0 ? 'dark' : 'light'}`);
    }).catch((e: Error) => failed('dark mode', e));
  }

  private fontScale(p: Plan): void {
    if (p.fontScale <= 0 || uiAppearance.getFontScale() === p.fontScale) {
      return;
    }
    uiAppearance.setFontScale(p.fontScale).then(() => {
      log(`font scale -> ${p.fontScale}`);
    }).catch((e: Error) => failed('font scale', e));
  }

  // The clock is set even when OpenHarmony shows it already, because
  // OpenHarmony's value can be the locale's default, which changes with the
  // locale.
  private clock(p: Plan): void {
    if (p.clock24 < 0) {
      return;
    }
    const want = p.clock24 === 1;
    i18n.System.set24HourClock(want);
    log(`clock -> ${want ? 24 : 12}-hour`);
  }

  // The desktop's high contrast becomes high-contrast text, and its reduced
  // motion turns animations off.
  private accessibility(p: Plan): void {
    if (p.highContrastText >= 0) {
      const on = p.highContrastText === 1;
      config.highContrastText.get().then((now: boolean) => now === on ? undefined :
        config.highContrastText.set(on).then(() => log(`high-contrast text -> ${on}`)))
        .catch((e: Error) => failed('high-contrast text', e));
    }
    if (p.animationOff >= 0) {
      const off = p.animationOff === 1;
      config.animationOff.get().then((now: boolean) => now === off ? undefined :
        config.animationOff.set(off).then(() => log(`animations off -> ${off}`)))
        .catch((e: Error) => failed('animations', e));
    }
  }

  private timezone(p: Plan): void {
    if (p.timezone.length === 0) {
      return;
    }
    systemDateTime.getTimezone().then((now: string) => now === p.timezone ? undefined :
      systemDateTime.setTimezone(p.timezone).then(() => log(`time zone -> ${p.timezone}`)))
      .catch((e: Error) => failed(`time zone ${p.timezone}`, e));
  }

  // The first of the desktop's languages that OpenHarmony offers becomes the
  // system language, and the region of the desktop's formats becomes the
  // region.
  private languages(p: Plan): void {
    if (p.languages.length === 0) {
      return;
    }
    const offered = i18n.System.getSystemLanguages();
    let language = '';
    for (const tag of p.languages) {
      language = i18n.I18NUtil.getBestMatchLocale(tag, offered);
      if (language.length > 0) {
        break;
      }
    }
    if (language.length === 0) {
      log(`no system language matches ${p.languages.join(',')}`);
    } else if (i18n.System.getSystemLanguage() !== language) {
      i18n.System.setSystemLanguage(language);
      log(`language -> ${language}`);
    }
    if (p.region.length > 0 && i18n.System.getSystemRegion() !== p.region) {
      i18n.System.setSystemRegion(p.region);
      log(`region -> ${p.region}`);
    }
  }

  // The week starts on LC_TIME's first day, and LC_MEASUREMENT gives the
  // measurement system and the temperature unit.
  private formats(p: Plan): void {
    if (p.weekLocale.length > 0) {
      const day = WEEK_DAYS[weekDayOf(i18n.getCalendar(p.weekLocale).getFirstDayOfWeek()) - 1];
      if (i18n.System.getFirstDayOfWeek() !== day) {
        i18n.System.setFirstDayOfWeek(day);
        log(`first day of the week -> ${day}`);
      }
    }
    if (p.measurement.length > 0 && i18n.System.getUsingMeasurement() !== p.measurement) {
      i18n.System.setSystemMeasurement(p.measurement);
      log(`measurement -> ${p.measurement}`);
    }
    if (p.temperature > 0) {
      const t = p.temperature === 2 ? i18n.TemperatureType.FAHRENHEIT : i18n.TemperatureType.CELSIUS;
      if (i18n.System.getTemperatureType() !== t) {
        i18n.System.setTemperatureType(t);
        log(`temperature -> ${p.temperature === 2 ? 'Fahrenheit' : 'Celsius'}`);
      }
    }
  }
}
