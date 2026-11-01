// The desktop's settings as the bridge sends them, and what OpenHarmony
// should change to follow them. This module imports no OpenHarmony API, so
// Node runs its tests (tests/desktop.test.mjs). HostSettings.ets applies the
// plan.

// One SETTINGS event (host/bridge/src/system/proto.rs). 0 and empty strings
// mean that the desktop has no value or that the profile doesn't follow it.
export class DesktopSettings {
  colorScheme: number = 0;
  contrast: number = 0;
  reducedMotion: number = 0;
  clock: number = 0;
  textScale: number = 0;
  timezone: string = '';
  locales: string = '';
  timeLocale: string = '';
  measureLocale: string = '';
}

// Reads a SETTINGS body, or returns undefined if it is too short. The bridge
// sends zone ids and BCP 47 tags in ASCII.
export function decodeSettings(body: ArrayBuffer): DesktopSettings | undefined {
  const v = new DataView(body);
  if (body.byteLength < 12) {
    return undefined;
  }
  let at = 12;
  const name = (): string | undefined => {
    if (at + 2 > body.byteLength) {
      return undefined;
    }
    const len = v.getUint16(at, true);
    at += 2;
    if (at + len > body.byteLength) {
      return undefined;
    }
    let s = '';
    for (let i = 0; i < len; i++) {
      s += String.fromCharCode(v.getUint8(at + i));
    }
    at += len;
    return s;
  };
  const s = new DesktopSettings();
  // Bytes 1 to 4 carry the accent, which OpenHarmony has no setting for.
  s.colorScheme = v.getUint8(0);
  s.contrast = v.getUint8(5);
  s.reducedMotion = v.getUint8(6);
  s.clock = v.getUint8(7);
  // The scale is an f32, which OpenHarmony would see as 1.100000023841858
  // for 1.1.
  s.textScale = Math.round(v.getFloat32(8, true) * 1000) / 1000;
  const tz = name();
  const locales = name();
  const time = name();
  const measure = name();
  if (tz === undefined || locales === undefined || time === undefined || measure === undefined) {
    return undefined;
  }
  s.timezone = tz;
  s.locales = locales;
  s.timeLocale = time;
  s.measureLocale = measure;
  return s;
}

// The changes that follow one SETTINGS event. -1, 0 and empty strings leave
// OpenHarmony's value as it is.
export class Plan {
  // uiAppearance.DarkMode: 0 dark, 1 light.
  darkMode: number = -1;
  // 1 for the 24-hour clock, 0 for the 12-hour clock.
  clock24: number = -1;
  fontScale: number = 0;
  highContrastText: number = -1;
  animationOff: number = -1;
  timezone: string = '';
  // The desktop's languages in order, as BCP 47 tags.
  languages: string[] = [];
  // The region of the desktop's formats, such as DE.
  region: string = '';
  // The locale whose first day of the week OpenHarmony takes.
  weekLocale: string = '';
  // i18n's measurement system id and TemperatureType (1 Celsius,
  // 2 Fahrenheit), from the desktop's LC_MEASUREMENT.
  measurement: string = '';
  temperature: number = 0;
}

// Regions that use US customary units and the UK's imperial units, from
// CLDR's measurementData.
const US_UNITS = ['US', 'LR', 'MM'];
const UK_UNITS = ['GB'];
// Regions that use Fahrenheit, from CLDR's unitPreferenceData.
const FAHRENHEIT = ['US', 'BS', 'BZ', 'KY', 'PR', 'PW'];

// The region of a BCP 47 tag. A tag without one, such as fr, takes its
// likely region.
export function regionOf(tag: string): string {
  try {
    const region = new Intl.Locale(tag).maximize().region;
    return region === undefined ? '' : region;
  } catch (e) {
    return '';
  }
}

// i18n's measurement system id for a region.
export function measurementOf(region: string): string {
  if (US_UNITS.includes(region)) {
    return 'ussystem';
  }
  return UK_UNITS.includes(region) ? 'uksystem' : 'metric';
}

// i18n's TemperatureType for a region.
export function temperatureOf(region: string): number {
  return FAHRENHEIT.includes(region) ? 2 : 1;
}

// i18n.WeekDay for a Calendar's first day, which counts from 1 for Sunday.
// WeekDay counts from 1 for Monday.
export function weekDayOf(calendarDay: number): number {
  return calendarDay === 1 ? 7 : calendarDay - 1;
}

// Plans the changes for `s`. `prev` is the last event that this connection
// applied. Without one, the plan holds every value that the desktop has,
// except normal contrast and full motion, which would undo a choice the user
// made in OpenHarmony at each boot. As on Android, a value applies when the
// desktop's value changes, so a change made in OpenHarmony lasts until then.
export function plan(prev: DesktopSettings | undefined, s: DesktopSettings): Plan {
  const p = new Plan();
  const first = prev === undefined;
  const changed = (f: (d: DesktopSettings) => number | string): boolean =>
    prev === undefined || f(prev) !== f(s);
  if (changed((d: DesktopSettings) => d.colorScheme) && s.colorScheme !== 0) {
    p.darkMode = s.colorScheme === 1 ? 0 : 1;
  }
  if (changed((d: DesktopSettings) => d.clock) && s.clock !== 0) {
    p.clock24 = s.clock === 2 ? 1 : 0;
  }
  if (changed((d: DesktopSettings) => d.textScale) && s.textScale > 0) {
    p.fontScale = s.textScale;
  }
  if (first ? s.contrast === 1 : changed((d: DesktopSettings) => d.contrast)) {
    p.highContrastText = s.contrast === 1 ? 1 : 0;
  }
  if (first ? s.reducedMotion === 1 : changed((d: DesktopSettings) => d.reducedMotion)) {
    p.animationOff = s.reducedMotion === 1 ? 1 : 0;
  }
  if (changed((d: DesktopSettings) => d.timezone)) {
    p.timezone = s.timezone;
  }
  const locale = changed((d: DesktopSettings) => d.locales) ||
    changed((d: DesktopSettings) => d.timeLocale) || changed((d: DesktopSettings) => d.measureLocale);
  if (locale) {
    p.languages = s.locales.split(',').filter((t: string) => t.length > 0);
    // OpenHarmony's region sets the formats, as LC_TIME's locale does on the
    // desktop. That locale falls back to the desktop's LANG.
    const formats = s.timeLocale.length > 0 ? s.timeLocale : p.languages.length > 0 ? p.languages[0] : '';
    p.region = formats.length > 0 ? regionOf(formats) : '';
    p.weekLocale = s.timeLocale;
    const region = s.measureLocale.length > 0 ? regionOf(s.measureLocale) : '';
    if (region.length > 0) {
      p.measurement = measurementOf(region);
      p.temperature = temperatureOf(region);
    }
  }
  return p;
}
