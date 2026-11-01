// Which keys the IME types itself, without OpenHarmony types, so
// ohos/system-app/tests runs it under Node.
//
// OpenHarmony maps the bridge's evdev keys with its own US keymap. Each key
// also reaches the IME as a bridge `key` line that carries what the key types
// under the host's layout, including composed text (host/bridge/src/input/
// proto.rs). The bridge sends the line before the key, so the line arrives
// first. When the key event reaches the IME, the IME consumes a key that types
// text and commits the host's text instead. Android's IME does the same in
// HostImeService.onKeyDown. Keys reach the IME before the editor and in order,
// so typing keeps its order.
//
// - A key without text, such as Enter, Backspace or an arrow, reaches the
//   editor.
// - A chord with Ctrl, Alt or Meta reaches the editor, so shortcuts work.
// - A step of a compose sequence types nothing.
// - OpenHarmony repeats a held key, and each repeat commits the text again.

import { decode } from './TextSession';

// Evdev codes and OpenHarmony's key codes (@ohos.multimodalInput.keyCode) of
// the keys that type text, as OpenHarmony's input service maps them.
const KEYS: number[][] = [
  // Digits 1 to 9 and 0.
  [2, 2001], [3, 2002], [4, 2003], [5, 2004], [6, 2005], [7, 2006], [8, 2007], [9, 2008], [10, 2009],
  [11, 2000],
  // Letters.
  [16, 2033], [17, 2039], [18, 2021], [19, 2034], [20, 2036], [21, 2041], [22, 2037], [23, 2025],
  [24, 2031], [25, 2032], [30, 2017], [31, 2035], [32, 2020], [33, 2022], [34, 2023], [35, 2024],
  [36, 2026], [37, 2027], [38, 2028], [44, 2042], [45, 2040], [46, 2019], [47, 2038], [48, 2018],
  [49, 2030], [50, 2029],
  // Punctuation and space.
  [12, 2057], [13, 2058], [26, 2059], [27, 2060], [39, 2062], [40, 2063], [41, 2056], [43, 2061],
  [51, 2043], [52, 2044], [53, 2064], [57, 2050], [86, 2602], [89, 2603], [124, 2615],
  // The keypad.
  [82, 2103], [79, 2104], [80, 2105], [81, 2106], [75, 2107], [76, 2108], [77, 2109], [71, 2110],
  [72, 2111], [73, 2112], [98, 2113], [55, 2114], [74, 2115], [78, 2116], [83, 2117], [121, 2118],
  [117, 2120],
];

const OH_KEY = new Map<number, number>();
for (const k of KEYS) {
  OH_KEY.set(k[0], k[1]);
}

// OpenHarmony's key code for an evdev key that can type text, or -1.
export function ohKey(evdev: number): number {
  const k = OH_KEY.get(evdev);
  return k === undefined ? -1 : k;
}

export class KeyText {
  // What the newest press of each key types under the host's layout, by
  // OpenHarmony key code. A compose step types ''.
  private pending = new Map<number, string>();
  // The held keys that the IME consumed, with their text.
  private consumed = new Map<number, string>();

  // Forgets the keys when the editor or the keyboard focus changes.
  reset(): void {
    this.pending.clear();
    this.consumed.clear();
  }

  // Reads a bridge line: `key <t> <code> 1 <sym> text <text>` or
  // `key <t> <code> 1 <sym> compose`. A release doesn't clear the text, because
  // its line can arrive before the press reaches the IME.
  onLine(line: string): void {
    const w = line.split(' ');
    if (w[0] === 'keyboard_enter' || w[0] === 'keyboard_leave') {
      this.pending.clear();
      return;
    }
    if (w[0] !== 'key' || w[3] !== '1') {
      return;
    }
    const code = ohKey(parseInt(w[2], 10));
    if (code < 0) {
      return;
    }
    let text: string | null = null;
    if (w.length === 7 && w[5] === 'text') {
      text = decode(w[6]);
    } else if (w.length === 6 && w[5] === 'compose') {
      text = '';
    }
    if (text === null) {
      this.pending.delete(code);
    } else {
      this.pending.set(code, text);
    }
  }

  // A key event from OpenHarmony. Returns null to pass the key to the editor,
  // or the text to commit when the IME consumes the key ('' commits nothing).
  // `chord` is true while Ctrl, Alt or Meta is held.
  onKey(code: number, down: boolean, chord: boolean): string | null {
    const held = this.consumed.get(code);
    if (!down) {
      if (held === undefined) {
        return null;
      }
      this.consumed.delete(code);
      return '';
    }
    if (held !== undefined) {
      return held;
    }
    const text = this.pending.get(code);
    if (text === undefined || chord) {
      return null;
    }
    this.pending.delete(code);
    this.consumed.set(code, text);
    return text;
  }
}
