import InputMethodExtensionAbility from '@ohos.InputMethodExtensionAbility';
import inputMethodEngine from '@ohos.inputMethodEngine';
import Want from '@ohos.app.ability.Want';
import util from '@ohos.util';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { onEvent, open, request } from 'libinnkeeper.so';
import { KeyEvent } from '@ohos.multimodalInput.keyEvent';
import { KeyText } from './KeyText';
import { Editor, TextSession, contentLine, cursorLine } from './TextSession';

// host/bridge/src/input/proto.rs
const INPUT = 6;
const TEXT_INPUT = 0x0604;
const INPUT_LINE = 0x4601;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperIme';
// UTF-16 units read on each side of the cursor.
const CONTEXT = 200;

class ClientEditor implements Editor {
  private client: inputMethodEngine.InputClient;
  private preview: boolean;

  constructor(client: inputMethodEngine.InputClient, preview: boolean) {
    this.client = client;
    this.preview = preview;
  }

  insertText(text: string): void {
    this.client.insertTextSync(text);
  }

  // OpenHarmony's deleteForward deletes before the cursor, and its
  // deleteBackward deletes after it.
  deleteBefore(length: number): void {
    this.client.deleteForwardSync(length);
  }

  deleteAfter(length: number): void {
    this.client.deleteBackwardSync(length);
  }

  setPreview(text: string): void {
    if (this.preview) {
      this.client.setPreviewTextSync(text, { start: -1, end: -1 });
    }
  }

  clearPreview(): void {
    if (this.preview) {
      this.client.setPreviewTextSync('', { start: -1, end: -1 });
      this.client.finishTextPreviewSync();
    }
  }
}

// "Innkeeper Keyboard" connects OpenHarmony's editors to the host's input
// method through the bridge's zwp_text_input_v3 (docs/input.md). It is the
// default input method (sys_prod/etc/param/innkeeper_ime.para) and creates no
// panel, so no soft keyboard covers the window. It types the keys that type
// text in the host's layout (KeyText).
export default class HostIme extends InputMethodExtensionAbility {
  private conn = -1;
  private session = new TextSession();
  private keys = new KeyText();
  private client: inputMethodEngine.InputClient | null = null;
  private editor: ClientEditor | null = null;
  private text = '';
  private selStart = 0;
  private selEnd = 0;
  private encoder = new util.TextEncoder();
  private decoder = util.TextDecoder.create('utf-8');

  onCreate(want: Want): void {
    this.conn = open(INPUT, 'innkeeper-ime');
    onEvent(this.conn, (type: number, body: ArrayBuffer, fds: number[]) => {
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === INPUT_LINE && body.byteLength >= 2) {
        const n = new DataView(body).getUint16(0, true);
        const line = this.decoder.decodeToString(new Uint8Array(body, 2, Math.min(n, body.byteLength - 2)));
        if (line.startsWith('ti_')) {
          try {
            this.session.onLine(line, this.editor);
          } catch (e) {
            hilog.error(DOMAIN, TAG, 'text input not applied: %{public}s', JSON.stringify(e));
          }
        } else {
          this.keys.onLine(line);
        }
      }
    });
    const ability = inputMethodEngine.getInputMethodAbility();
    ability.on('inputStart', (kb: inputMethodEngine.KeyboardController, client: inputMethodEngine.InputClient) => {
      this.start(client);
    });
    ability.on('inputStop', () => {
      this.stop();
    });
    const keys = inputMethodEngine.getKeyboardDelegate();
    keys.on('keyEvent', (e: KeyEvent) => this.key(e));
    keys.on('cursorContextChange', (x: number, y: number, height: number) => {
      if (this.client !== null) {
        this.send([cursorLine(x, y, height), 'ti_apply']);
      }
    });
    keys.on('selectionChange', (oldBegin: number, oldEnd: number, newBegin: number, newEnd: number) => {
      this.selStart = Math.min(newBegin, newEnd);
      this.selEnd = Math.max(newBegin, newEnd);
      this.surrounding();
    });
    keys.on('textChange', (text: string) => {
      this.text = text;
      this.surrounding();
    });
    keys.on('editorAttributeChanged', (attr: inputMethodEngine.EditorAttribute) => {
      if (this.client !== null) {
        this.send([contentLine(attr.inputPattern, attr.enterKeyType, attr.capitalizeMode ?? 0), 'ti_apply']);
      }
    });
    hilog.info(DOMAIN, TAG, 'created');
  }

  onDestroy(): void {
    this.stop();
  }

  // Consumes a key that types text and commits the host's text for it.
  private key(e: KeyEvent): boolean {
    const editor = this.editor;
    if (editor === null || (e.action !== 1 && e.action !== 2)) {
      return false;
    }
    const text = this.keys.onKey(e.key.code, e.action === 1, e.ctrlKey || e.altKey || e.logoKey);
    if (text === null) {
      return false;
    }
    if (text.length > 0) {
      try {
        editor.insertText(text);
      } catch (err) {
        hilog.error(DOMAIN, TAG, 'insertText: %{public}s', JSON.stringify(err));
      }
    }
    return true;
  }

  private start(client: inputMethodEngine.InputClient): void {
    this.session.reset();
    this.keys.reset();
    this.text = '';
    this.selStart = 0;
    this.selEnd = 0;
    let attr: inputMethodEngine.EditorAttribute;
    try {
      attr = client.getEditorAttributeSync();
    } catch (e) {
      hilog.error(DOMAIN, TAG, 'getEditorAttribute: %{public}s', JSON.stringify(e));
      return;
    }
    this.client = client;
    this.editor = new ClientEditor(client, attr.isTextPreviewSupported);
    const lines = ['ti_enable', contentLine(attr.inputPattern, attr.enterKeyType, attr.capitalizeMode ?? 0)];
    const s = this.surroundingLine();
    if (s !== null) {
      lines.push(s);
    }
    lines.push('ti_apply');
    this.send(lines);
    hilog.info(DOMAIN, TAG, 'input started, pattern %{public}d', attr.inputPattern);
  }

  private stop(): void {
    if (this.client !== null) {
      this.send(['ti_disable', 'ti_apply']);
    }
    this.client = null;
    this.editor = null;
    this.session.reset();
    this.keys.reset();
  }

  private surrounding(): void {
    const s = this.surroundingLine();
    if (s !== null) {
      this.send([s, 'ti_apply']);
    }
  }

  // The editor's text around the cursor. textChange gives the whole text,
  // which has the selection. Before the first textChange, the text comes
  // from the client without the selection.
  private surroundingLine(): string | null {
    const client = this.client;
    if (client === null) {
      return null;
    }
    if (this.selEnd <= this.text.length && this.text.length > 0) {
      return this.session.surroundingLine(this.text.substring(0, this.selStart),
        this.text.substring(this.selStart, this.selEnd), this.text.substring(this.selEnd));
    }
    try {
      return this.session.surroundingLine(client.getForwardSync(CONTEXT), '', client.getBackwardSync(CONTEXT));
    } catch (e) {
      return null;
    }
  }

  // Each line is one TEXT_INPUT request, sent in order.
  private send(lines: string[]): void {
    for (const line of lines) {
      const bytes = this.encoder.encodeInto(line);
      const body = new Uint8Array(2 + bytes.length);
      new DataView(body.buffer).setUint16(0, bytes.length, true);
      body.set(bytes, 2);
      request(this.conn, TEXT_INPUT, body.buffer, []).catch((e: Error) => {
        hilog.info(DOMAIN, TAG, 'text input not sent: %{public}s', e.message);
      });
    }
  }
}
