// Host tests for ets/settings/Desktop.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const {
  DesktopSettings,
  decodeSettings,
  plan,
  regionOf,
  measurementOf,
  temperatureOf,
  weekDayOf,
} = await load("Desktop", "settings");

// A SETTINGS body as the bridge's Settings::encode writes it.
function body(s) {
  const names = [s.timezone, s.locales, s.timeLocale, s.measureLocale];
  const bytes = new Uint8Array(
    12 + names.reduce((n, x) => n + 2 + x.length, 0),
  );
  const v = new DataView(bytes.buffer);
  v.setUint8(0, s.colorScheme);
  // An accent, which OpenHarmony ignores.
  v.setUint8(1, 1);
  v.setUint8(2, 0x3d);
  v.setUint8(5, s.contrast);
  v.setUint8(6, s.reducedMotion);
  v.setUint8(7, s.clock);
  v.setFloat32(8, s.textScale, true);
  let at = 12;
  for (const n of names) {
    v.setUint16(at, n.length, true);
    at += 2;
    for (const c of n) bytes[at++] = c.charCodeAt(0);
  }
  return bytes.buffer;
}

function desktop(fields) {
  return Object.assign(new DesktopSettings(), fields);
}

const berlin = desktop({
  colorScheme: 1,
  contrast: 1,
  clock: 2,
  textScale: 1.25,
  timezone: "Europe/Berlin",
  locales: "de-DE,en-US",
  timeLocale: "de-DE",
  measureLocale: "en-US",
});

test("a SETTINGS body decodes", () => {
  assert.deepEqual({ ...decodeSettings(body(berlin)) }, { ...berlin });
});

test("the text scale loses the f32's rounding", () => {
  assert.equal(
    decodeSettings(body(desktop({ textScale: 1.1 }))).textScale,
    1.1,
  );
});

test("a short or truncated body is refused", () => {
  assert.equal(decodeSettings(new ArrayBuffer(11)), undefined);
  const full = body(berlin);
  assert.equal(decodeSettings(full.slice(0, full.byteLength - 1)), undefined);
  assert.equal(decodeSettings(full.slice(0, 13)), undefined);
});

test("the first event applies each value that the desktop has", () => {
  const p = plan(undefined, berlin);
  assert.equal(p.darkMode, 0);
  assert.equal(p.clock24, 1);
  assert.equal(p.fontScale, 1.25);
  assert.equal(p.highContrastText, 1);
  assert.equal(p.animationOff, -1);
  assert.equal(p.timezone, "Europe/Berlin");
  assert.deepEqual(p.languages, ["de-DE", "en-US"]);
  assert.equal(p.region, "DE");
  assert.equal(p.weekLocale, "de-DE");
  assert.equal(p.measurement, "ussystem");
  assert.equal(p.temperature, 2);
});

test("values that the desktop lacks or doesn't share leave OpenHarmony's", () => {
  const p = plan(undefined, new DesktopSettings());
  assert.equal(p.darkMode, -1);
  assert.equal(p.clock24, -1);
  assert.equal(p.fontScale, 0);
  // Normal contrast and full motion at connection don't undo a choice made
  // in OpenHarmony.
  assert.equal(p.highContrastText, -1);
  assert.equal(p.animationOff, -1);
  assert.deepEqual(p.languages, []);
  assert.equal(p.region, "");
  assert.equal(p.weekLocale, "");
  assert.equal(p.measurement, "");
  assert.equal(p.temperature, 0);
});

test("a later event applies what changed", () => {
  const next = desktop({ ...berlin, colorScheme: 2, contrast: 0 });
  const p = plan(berlin, next);
  assert.equal(p.darkMode, 1);
  assert.equal(p.highContrastText, 0);
  assert.equal(p.clock24, -1);
  assert.equal(p.fontScale, 0);
  assert.equal(p.timezone, "");
  assert.deepEqual(p.languages, []);
  // The desktop's light preference changing to none keeps OpenHarmony's.
  assert.equal(plan(next, desktop({ ...next, colorScheme: 0 })).darkMode, -1);
  assert.equal(plan(berlin, desktop({ ...berlin, clock: 1 })).clock24, 0);
  assert.equal(
    plan(berlin, desktop({ ...berlin, reducedMotion: 1 })).animationOff,
    1,
  );
});

test("a change of formats plans the whole locale", () => {
  const p = plan(berlin, desktop({ ...berlin, timeLocale: "en-GB" }));
  assert.deepEqual(p.languages, ["de-DE", "en-US"]);
  assert.equal(p.region, "GB");
  assert.equal(p.weekLocale, "en-GB");
  assert.equal(p.measurement, "ussystem");
});

test("the region comes from the formats, then from the first language", () => {
  const fr = desktop({ locales: "fr,en-US" });
  assert.equal(plan(undefined, fr).region, "FR");
  assert.equal(
    plan(undefined, desktop({ ...fr, timeLocale: "en-CA" })).region,
    "CA",
  );
  assert.equal(regionOf("sr-Latn-RS"), "RS");
  assert.equal(regionOf("zh-Hant"), "TW");
  assert.equal(regionOf("not a tag"), "");
});

test("regions map to i18n's measurement systems and temperature units", () => {
  assert.equal(measurementOf("US"), "ussystem");
  assert.equal(measurementOf("GB"), "uksystem");
  assert.equal(measurementOf("DE"), "metric");
  assert.equal(temperatureOf("US"), 2);
  assert.equal(temperatureOf("GB"), 1);
  const uk = plan(undefined, desktop({ measureLocale: "en-GB" }));
  assert.equal(uk.measurement, "uksystem");
  assert.equal(uk.temperature, 1);
});

test("a calendar's first day maps to i18n's WeekDay", () => {
  // Sunday, Monday and Saturday.
  assert.equal(weekDayOf(1), 7);
  assert.equal(weekDayOf(2), 1);
  assert.equal(weekDayOf(7), 6);
});
