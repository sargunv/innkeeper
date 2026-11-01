// The host input method's side of an editor, without OpenHarmony types, so
// ohos/system-app/tests runs it under Node. It builds the `ti_*` lines that
// the bridge parses (host/bridge/src/input/ime.rs) and applies the bridge's
// lines to an Editor. Android's system app has the same logic in
// ime/TextInputSession.kt.

// The InputClient calls that the session makes. Lengths are UTF-16 units.
export interface Editor {
  insertText(text: string): void;
  deleteBefore(length: number): void;
  deleteAfter(length: number): void;
  // Shows `text` as preview text in place of the current preview.
  setPreview(text: string): void;
  // Removes the preview text.
  clearPreview(): void;
}

// What the bridge accepts in one line (ime.rs MAX_LINE is 480).
export const MAX_LINE = 470;
// UTF-16 units of context on each side of the selection.
const MAX_CONTEXT = 120;

// OpenHarmony's EditorAttribute.inputPattern (inputMethodEngine.PATTERN_*).
// ArkUI's TextInput of the normal type reports PATTERN_NULL.
export const PATTERN_NULL = -1;
export const PATTERN_TEXT = 0;
export const PATTERN_NUMBER = 2;
export const PATTERN_PHONE = 3;
export const PATTERN_DATETIME = 4;
export const PATTERN_EMAIL = 5;
export const PATTERN_URI = 6;
export const PATTERN_PASSWORD = 7;
export const PATTERN_PASSWORD_NUMBER = 8;
export const PATTERN_PASSWORD_SCREEN_LOCK = 9;
export const PATTERN_USER_NAME = 10;
export const PATTERN_NEW_PASSWORD = 11;
export const PATTERN_NUMBER_DECIMAL = 12;
export const PATTERN_ONE_TIME_CODE = 13;
// EditorAttribute.enterKeyType
export const ENTER_KEY_TYPE_NEWLINE = 8;
// EditorAttribute.capitalizeMode
export const CAPITALIZE_SENTENCES = 1;
export const CAPITALIZE_WORDS = 2;
export const CAPITALIZE_CHARACTERS = 3;

// zwp_text_input_v3.content_hint
const HINT_AUTO_CAPITALIZATION = 0x4;
const HINT_UPPERCASE = 0x10;
const HINT_TITLECASE = 0x20;
const HINT_HIDDEN_TEXT = 0x40;
const HINT_SENSITIVE_DATA = 0x80;
const HINT_MULTILINE = 0x200;
// zwp_text_input_v3.content_purpose
const PURPOSE_NORMAL = 0;
const PURPOSE_DIGITS = 2;
const PURPOSE_NUMBER = 3;
const PURPOSE_PHONE = 4;
const PURPOSE_URL = 5;
const PURPOSE_EMAIL = 6;
const PURPOSE_PASSWORD = 8;
const PURPOSE_PIN = 9;
const PURPOSE_DATETIME = 12;

// The `ti_content <hint> <purpose>` line for an editor.
export function contentLine(pattern: number, enterKeyType: number, capitalize: number): string {
  let hint = 0;
  let purpose = PURPOSE_NORMAL;
  switch (pattern) {
    case PATTERN_NUMBER:
      purpose = PURPOSE_DIGITS;
      break;
    case PATTERN_NUMBER_DECIMAL:
      purpose = PURPOSE_NUMBER;
      break;
    case PATTERN_PHONE:
      purpose = PURPOSE_PHONE;
      break;
    case PATTERN_DATETIME:
      purpose = PURPOSE_DATETIME;
      break;
    case PATTERN_EMAIL:
      purpose = PURPOSE_EMAIL;
      break;
    case PATTERN_URI:
      purpose = PURPOSE_URL;
      break;
    case PATTERN_PASSWORD:
    case PATTERN_PASSWORD_SCREEN_LOCK:
    case PATTERN_NEW_PASSWORD:
      purpose = PURPOSE_PASSWORD;
      hint |= HINT_HIDDEN_TEXT | HINT_SENSITIVE_DATA;
      break;
    case PATTERN_PASSWORD_NUMBER:
      purpose = PURPOSE_PIN;
      hint |= HINT_HIDDEN_TEXT | HINT_SENSITIVE_DATA;
      break;
    case PATTERN_ONE_TIME_CODE:
      purpose = PURPOSE_DIGITS;
      hint |= HINT_SENSITIVE_DATA;
      break;
    default:
      break;
  }
  if (purpose === PURPOSE_NORMAL) {
    if (capitalize === CAPITALIZE_SENTENCES) {
      hint |= HINT_AUTO_CAPITALIZATION;
    } else if (capitalize === CAPITALIZE_WORDS) {
      hint |= HINT_TITLECASE;
    } else if (capitalize === CAPITALIZE_CHARACTERS) {
      hint |= HINT_UPPERCASE;
    }
    if (enterKeyType === ENTER_KEY_TYPE_NEWLINE) {
      hint |= HINT_MULTILINE;
    }
  }
  return `ti_content ${hint} ${purpose}`;
}

// The `ti_cursor` line for the text cursor that cursorContextChange reports:
// its left, top and height in display pixels. The SDK's declaration calls y
// the bottom, and this release passes the top.
export function cursorLine(x: number, y: number, height: number): string {
  return `ti_cursor ${Math.round(x)} ${Math.round(y)} 1 ${Math.max(1, Math.round(height))}`;
}

function utf8Length(cp: number): number {
  if (cp < 0x80) {
    return 1;
  }
  if (cp < 0x800) {
    return 2;
  }
  return cp < 0x10000 ? 3 : 4;
}

// The UTF-8 length of a string.
export function utf8(s: string): number {
  let n = 0;
  let i = 0;
  while (i < s.length) {
    const cp = s.codePointAt(i) as number;
    n += utf8Length(cp);
    i += cp > 0xffff ? 2 : 1;
  }
  return n;
}

function hex(c: number): string {
  return '%' + (c < 16 ? '0' : '') + c.toString(16).toUpperCase();
}

// Percent-encodes text as ime.rs does: bytes up to and including space, DEL
// and '%' become %XX, and everything else is sent as is.
export function encode(s: string): string {
  let out = '';
  for (let i = 0; i < s.length; i++) {
    const c = s.charCodeAt(i);
    out += c <= 0x20 || c === 0x7f || c === 0x25 ? hex(c) : s.charAt(i);
  }
  return out;
}

// Decodes the bridge's percent-encoded text, or returns null if it is
// malformed.
export function decode(s: string): string | null {
  let out = '';
  let i = 0;
  while (i < s.length) {
    if (s.charAt(i) !== '%') {
      out += s.charAt(i);
      i++;
      continue;
    }
    const h = s.substring(i + 1, i + 3);
    if (!/^[0-9A-F]{2}$/.test(h)) {
      return null;
    }
    out += String.fromCharCode(parseInt(h, 16));
    i += 3;
  }
  return out;
}

function isLow(c: number): boolean {
  return c >= 0xdc00 && c <= 0xdfff;
}

function isHigh(c: number): boolean {
  return c >= 0xd800 && c <= 0xdbff;
}

export class TextSession {
  // The surrounding text last sent, which the bridge's byte lengths refer
  // to, and the selection in it as UTF-16 indices.
  private sentText = '';
  private sentStart = 0;
  private sentEnd = 0;
  private deleteBefore = -1;
  private deleteAfter = -1;
  private commit = '';
  private hasCommit = false;
  private preedit = '';
  // Whether the editor shows a preview.
  private previewing = false;
  // Set when a batch changed the text, so the next text update counts as the
  // input method's.
  private ownEdit = false;

  // Forgets the old editor's state when a new editor starts.
  reset(): void {
    this.sentText = '';
    this.sentStart = 0;
    this.sentEnd = 0;
    this.clearBatch();
    this.previewing = false;
    this.ownEdit = false;
  }

  private clearBatch(): void {
    this.deleteBefore = -1;
    this.deleteAfter = -1;
    this.commit = '';
    this.hasCommit = false;
    this.preedit = '';
  }

  // The `ti_surrounding` line for the selection `selected` between `before`
  // and `after`, trimmed around the selection until the line fits. The change
  // counts as the input method's if it follows one of its edits, and
  // otherwise as "other", which resets the host input method.
  surroundingLine(before: string, selected: string, after: string): string {
    const cause = this.ownEdit ? 0 : 1;
    this.ownEdit = false;
    let keep = MAX_CONTEXT;
    while (true) {
      let from = Math.max(0, before.length - keep);
      let to = Math.min(after.length, keep);
      // Never split a surrogate pair.
      if (from > 0 && from < before.length && isLow(before.charCodeAt(from))) {
        from++;
      }
      if (to > 0 && to < after.length && isHigh(after.charCodeAt(to - 1))) {
        to--;
      }
      const head = before.substring(from);
      const text = head + selected + after.substring(0, to);
      const line = `ti_surrounding ${utf8(head + selected)} ${utf8(head)} ${cause} ${encode(text)}`;
      if (utf8(line) <= MAX_LINE) {
        this.sentText = text;
        this.sentStart = head.length;
        this.sentEnd = head.length + selected.length;
        return line;
      }
      if (keep === 0) {
        // If the selection alone doesn't fit, send the cursor without text.
        this.sentText = '';
        this.sentStart = 0;
        this.sentEnd = 0;
        return `ti_surrounding 0 0 ${cause} `;
      }
      keep = Math.floor(keep / 2);
    }
  }

  // Gathers one line from the bridge, and applies the batch to `editor` at
  // `ti_done`. Returns false for a line that isn't a text-input result.
  onLine(line: string, editor: Editor | null): boolean {
    const space = line.indexOf(' ');
    const op = space < 0 ? line : line.substring(0, space);
    const rest = space < 0 ? '' : line.substring(space + 1);
    const words = rest.split(' ');
    switch (op) {
      case 'ti_delete':
        this.deleteBefore = parseInt(words[0], 10);
        this.deleteAfter = parseInt(words[1], 10);
        return true;
      case 'ti_commit': {
        const t = decode(rest);
        if (t !== null) {
          this.commit += t;
          this.hasCommit = true;
        }
        return true;
      }
      case 'ti_preedit': {
        const t = words.length > 2 ? decode(words[2]) : '';
        this.preedit = t === null ? '' : t;
        return true;
      }
      case 'ti_done':
        if (editor !== null) {
          this.apply(editor);
        }
        this.clearBatch();
        return true;
      default:
        return false;
    }
  }

  // This follows zwp_text_input_v3's order: remove the preview, delete
  // around the cursor, insert the commit, and show the new preview.
  private apply(ed: Editor): void {
    const deletes = this.deleteBefore > 0 || this.deleteAfter > 0;
    this.ownEdit = deletes || this.hasCommit || this.preedit.length > 0 || this.previewing;
    if (this.previewing && (deletes || this.hasCommit || this.preedit.length === 0)) {
      ed.clearPreview();
      this.previewing = false;
    }
    if (deletes) {
      if (this.deleteBefore > 0) {
        ed.deleteBefore(this.before(this.deleteBefore));
      }
      if (this.deleteAfter > 0) {
        ed.deleteAfter(this.after(this.deleteAfter));
      }
    }
    if (this.hasCommit && this.commit.length > 0) {
      ed.insertText(this.commit);
    }
    if (this.preedit.length > 0) {
      ed.setPreview(this.preedit);
      this.previewing = true;
    }
  }

  // UTF-16 units for `bytes` of UTF-8 before the selection last sent.
  // Beyond the sent text, each byte counts as one unit.
  private before(bytes: number): number {
    let i = Math.min(this.sentStart, this.sentText.length);
    let n = 0;
    while (i > 0 && n < bytes) {
      const units = i > 1 && isLow(this.sentText.charCodeAt(i - 1)) && isHigh(this.sentText.charCodeAt(i - 2)) ? 2 : 1;
      n += utf8Length(this.sentText.codePointAt(i - units) as number);
      i -= units;
    }
    return this.sentStart - i + Math.max(0, bytes - n);
  }

  // UTF-16 units for `bytes` of UTF-8 after the selection last sent.
  private after(bytes: number): number {
    let i = this.sentEnd;
    let n = 0;
    while (i < this.sentText.length && n < bytes) {
      const cp = this.sentText.codePointAt(i) as number;
      n += utf8Length(cp);
      i += cp > 0xffff ? 2 : 1;
    }
    return i - this.sentEnd + Math.max(0, bytes - n);
  }
}
