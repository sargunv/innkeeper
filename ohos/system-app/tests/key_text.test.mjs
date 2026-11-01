// Host tests for ets/ime/KeyText.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const { KeyText, ohKey } = await load("KeyText");

// OpenHarmony's key codes (@ohos.multimodalInput.keyCode).
const A = 2017;
const Q = 2033;
const DIGIT_2 = 2002;
const LEFT_BRACKET = 2059;
const SPACE = 2050;

test("evdev keys map to OpenHarmony's key codes", () => {
  assert.equal(ohKey(30), A);
  assert.equal(ohKey(16), Q);
  assert.equal(ohKey(3), DIGIT_2);
  assert.equal(ohKey(11), 2000);
  assert.equal(ohKey(57), SPACE);
  assert.equal(ohKey(82), 2103);
  // Enter and Backspace never type text.
  assert.equal(ohKey(28), -1);
  assert.equal(ohKey(14), -1);
});

test("a key that types text commits the host's text", () => {
  const k = new KeyText();
  // AZERTY's Q and 2 keys.
  k.onLine("key 1 16 1 97 text a");
  k.onLine("key 2 16 0");
  k.onLine("key 3 3 1 233 text é");
  assert.equal(k.onKey(Q, true, false), "a");
  assert.equal(k.onKey(Q, false, false), "");
  assert.equal(k.onKey(DIGIT_2, true, false), "é");
  assert.equal(k.onKey(DIGIT_2, false, false), "");
  k.onLine("key 4 57 1 32 text %20");
  assert.equal(k.onKey(SPACE, true, false), " ");
});

test("keys without text, chords and unknown keys reach the editor", () => {
  const k = new KeyText();
  k.onLine("key 1 28 1 65293");
  assert.equal(k.onKey(2054, true, false), null);
  k.onLine("key 2 30 1 97 text a");
  assert.equal(k.onKey(A, true, true), null);
  assert.equal(k.onKey(A, false, true), null);
  // A key whose line hasn't arrived types OpenHarmony's own text.
  assert.equal(k.onKey(Q, true, false), null);
  // A malformed line types nothing of its own.
  k.onLine("key 3 16 1 97 text %2");
  assert.equal(k.onKey(Q, true, false), null);
});

test("a compose step types nothing and its result commits", () => {
  const k = new KeyText();
  k.onLine("key 1 26 1 65106 compose");
  assert.equal(k.onKey(LEFT_BRACKET, true, false), "");
  assert.equal(k.onKey(LEFT_BRACKET, false, false), "");
  k.onLine("key 2 18 1 101 text ê");
  assert.equal(k.onKey(2021, true, false), "ê");
});

test("OpenHarmony's repeats commit the text again until the release", () => {
  const k = new KeyText();
  k.onLine("key 1 30 1 97 text a");
  // The release line can arrive before the key's events reach the IME.
  k.onLine("key 2 30 0");
  assert.equal(k.onKey(A, true, false), "a");
  assert.equal(k.onKey(A, true, false), "a");
  // A chord pressed during the repeat doesn't change the held key.
  assert.equal(k.onKey(A, true, true), "a");
  assert.equal(k.onKey(A, false, false), "");
  // Each press needs its own line.
  assert.equal(k.onKey(A, true, false), null);
  // The bridge's repeat lines don't add text.
  k.onLine("key 3 30 2 97 text a");
  assert.equal(k.onKey(A, true, false), null);
});

test("a focus change or a new editor forgets the keys", () => {
  const k = new KeyText();
  k.onLine("key 1 30 1 97 text a");
  k.onLine("keyboard_leave 2147483649");
  assert.equal(k.onKey(A, true, false), null);
  k.onLine("key 2 30 1 97 text a");
  assert.equal(k.onKey(A, true, false), "a");
  k.reset();
  assert.equal(k.onKey(A, false, false), null);
});
