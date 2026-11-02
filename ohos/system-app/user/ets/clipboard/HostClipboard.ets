import common from '@ohos.app.ability.common';
import fileUri from '@ohos.file.fileuri';
import image from '@ohos.multimedia.image';
import pasteboard from '@ohos.pasteboard';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, Reply, request } from 'libinnkeeper.so';
import { mimeOf } from '../files/Mime';
import {
  BGRA, choose, Choice, ClipFile, ClipHeader, decodeFiles, decodeImage, Echo, encodeFiles, encodeHeader, encodeImage,
  FILES, fileMime, fileOverhead, fitScale, hash, IMAGE, Item, MAX_FILE_BYTES, MAX_FILE_CLIP, MAX_TEXT, MIMETYPE_TEXT_URI,
  Rgba, RGBA, TAG, TEXT, toRgba, truncateUtf8, uriName, Utf8,
} from './Clip';

// host/bridge/src/system/proto.rs
const CLIP_SET = 0x0304;
const DOMAIN = 0x0000;
const LOG_TAG = 'InnkeeperClipboard';

const encoder = new util.TextEncoder();
const decoder = util.TextDecoder.create('utf-8', { fatal: true, ignoreBOM: true });

// UTF-8 conversion with OpenHarmony's util.TextEncoder and TextDecoder.
class OhosUtf8 implements Utf8 {
  encode(s: string): Uint8Array {
    // OpenHarmony's encoder returns nothing for an empty string.
    return s.length === 0 ? new Uint8Array(0) : encoder.encodeInto(s);
  }

  decode(b: Uint8Array): string | undefined {
    try {
      return decoder.decodeToString(b);
    } catch (e) {
      return undefined;
    }
  }
}

const utf8 = new OhosUtf8();

function log(what: string): void {
  hilog.info(DOMAIN, LOG_TAG, '%{public}s', what);
}

function failed(what: string, e: Error): void {
  hilog.warn(DOMAIN, LOG_TAG, '%{public}s: %{public}s', what, e.message);
}

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

// Reads `len` bytes from the start of the file `fd`, such as a blob's memfd. Each read fills the
// start of the buffer that it gets, so the reads go through a chunk.
function readAll(fd: number, len: number): Uint8Array {
  const out = new Uint8Array(len);
  const chunk = new ArrayBuffer(Math.min(len, 1 << 20));
  let at = 0;
  while (at < len) {
    const n = fileIo.readSync(fd, chunk, { offset: at, length: Math.min(len - at, chunk.byteLength) });
    if (n <= 0) {
      throw new Error(`the file ends at ${at} of ${len} bytes`);
    }
    out.set(new Uint8Array(chunk, 0, n), at);
    at += n;
  }
  return out;
}

// Reads the file at a file URI that the pasteboard granted, if it has at
// most `max` bytes.
function readUri(uri: string, max: number): Uint8Array {
  const f = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
  try {
    const size = fileIo.statSync(f.fd).size;
    if (size > max) {
      throw new Error(`${uriName(uri)} has ${size} bytes, over the clipboard's cap`);
    }
    return readAll(f.fd, size);
  } finally {
    fileIo.closeSync(f);
  }
}

// The PixelMap's pixels as straight RGBA, at most MAX_SIDE on a side.
async function pixels(pm: image.PixelMap): Promise<Rgba> {
  let info = pm.getImageInfoSync();
  const scale = fitScale(info.size.width, info.size.height);
  if (scale < 1) {
    pm = pm.createScaledPixelMapSync(scale, scale);
    info = pm.getImageInfoSync();
  }
  if (info.pixelFormat !== image.PixelMapFormat.RGBA_8888 && info.pixelFormat !== image.PixelMapFormat.BGRA_8888) {
    await pm.convertPixelFormat(image.PixelMapFormat.RGBA_8888);
    info = pm.getImageInfoSync();
  }
  const buf = new ArrayBuffer(pm.getPixelBytesNumber());
  pm.readPixelsToBufferSync(buf);
  const layout = info.pixelFormat === image.PixelMapFormat.BGRA_8888 ? BGRA : RGBA;
  return toRgba(new Uint8Array(buf), info.size.width, info.size.height, info.stride, layout,
    info.alphaType === image.AlphaType.PREMUL);
}

function textKey(b: Uint8Array): string {
  return `text ${hash(b)}`;
}

function imageKey(img: Rgba): string {
  return `image ${img.width}x${img.height} ${hash(img.pixels)}`;
}

function filesKey(files: ClipFile[]): string {
  return `files ${files.map((f: ClipFile) => `${f.name} ${hash(f.bytes)}`).join(' ')}`;
}

// Syncs OpenHarmony's pasteboard with the desktop's clipboard, as Android's
// ClipboardSync does. The bridge sends each new desktop clip as a CLIP
// event, and the app sends each new pasteboard clip in CLIP_SET.
//
// The app holds READ_PASTEBOARD, so it reads the pasteboard while other apps
// have focus, as Android's system app reads the clipboard. The pasteboard
// keeps a clip for each user, and the app runs as the foreground user.
//
// A desktop clip of files becomes copies in the app's files and a
// pasteboard clip of their URIs. The pasteboard grants the app that pastes
// the URIs, and it opens them through the user's appdata view. A pasteboard
// clip of file URIs from an app goes to the desktop as the files' bytes,
// which the app reads with the pasteboard's grant.
export class HostClipboard {
  private board: pasteboard.SystemPasteboard = pasteboard.getSystemPasteboard();
  private system: number;
  private echo: Echo = new Echo();
  private queue: Promise<void> = Promise.resolve();
  // The desktop's file clips, one folder each. A new clip replaces the
  // folder.
  private clips: string;
  private clip: number = 0;

  constructor(context: common.ServiceExtensionContext, system: number) {
    this.system = system;
    this.clips = `${context.filesDir}/clip`;
    this.removeClips();
  }

  private removeClips(): void {
    try {
      fileIo.rmdirSync(this.clips);
    } catch (e) {
      // There is no earlier clip.
    }
  }

  // Copies the desktop's files and returns a pasteboard clip of their URIs.
  private filesData(files: ClipFile[]): pasteboard.PasteData {
    this.removeClips();
    const dir = `${this.clips}/${++this.clip}`;
    const uris = files.map((f: ClipFile, i: number) => {
      // Each file gets its own folder, so two files may have one name.
      fileIo.mkdirSync(`${dir}/${i}`, true);
      const path = `${dir}/${i}/${f.name}`;
      const out = fileIo.openSync(path, fileIo.OpenMode.CREATE | fileIo.OpenMode.WRITE_ONLY | fileIo.OpenMode.TRUNC);
      try {
        fileIo.writeSync(out.fd, f.bytes.buffer.slice(f.bytes.byteOffset, f.bytes.byteOffset + f.bytes.length));
      } finally {
        fileIo.closeSync(out);
      }
      return fileUri.getUriFromPath(path);
    });
    const data = pasteboard.createData(MIMETYPE_TEXT_URI, uris[0]);
    uris.slice(1).forEach((u: string) => data.addRecord(MIMETYPE_TEXT_URI, u));
    return data;
  }

  watch(): void {
    this.board.on('update', () => this.enqueue(() => this.toDesktop()));
  }

  // Takes a CLIP event and the blob that came with it.
  fromDesktop(h: ClipHeader, fd: number): void {
    let b: Uint8Array;
    try {
      b = readAll(fd, h.len);
    } catch (e) {
      failed('reading the desktop clip', e as Error);
      return;
    } finally {
      fileIo.closeSync(fd);
    }
    this.enqueue(() => this.set(h.kind, b));
  }

  private enqueue(job: () => Promise<void>): void {
    this.queue = this.queue.then(job).catch((e: Error) => failed('clipboard', e));
  }

  private async set(kind: number, b: Uint8Array): Promise<void> {
    let data: pasteboard.PasteData;
    // The identity that toDesktop would give the clip.
    let key = '';
    if (kind === TEXT) {
      const text = utf8.decode(b);
      if (text === undefined) {
        return;
      }
      key = textKey(b);
      data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, text);
    } else if (kind === IMAGE) {
      const img = decodeImage(b);
      if (img === undefined) {
        return;
      }
      key = imageKey(img);
      const copy = img.pixels.slice();
      const pm = await image.createPixelMap(copy.buffer, {
        size: { width: img.width, height: img.height },
        srcPixelFormat: image.PixelMapFormat.RGBA_8888,
        pixelFormat: image.PixelMapFormat.RGBA_8888,
        alphaType: image.AlphaType.UNPREMUL,
      });
      data = pasteboard.createData(pasteboard.MIMETYPE_PIXELMAP, pm);
    } else if (kind === FILES) {
      const files = decodeFiles(b, utf8);
      if (files === undefined) {
        return;
      }
      key = filesKey(files);
      data = this.filesData(files);
    } else {
      return;
    }
    const p = data.getProperty();
    p.tag = TAG;
    p.shareOption = pasteboard.ShareOption.LOCALDEVICE;
    data.setProperty(p);
    this.echo.incoming(key);
    await this.board.setData(data);
    log(`clip from the desktop (kind ${kind}, ${b.length} bytes)`);
  }

  private async toDesktop(): Promise<void> {
    const data = await this.board.getData();
    const items: Item[] = [];
    for (let i = 0; i < data.getRecordCount(); i++) {
      const r = data.getRecord(i);
      const item = new Item();
      item.mime = r.mimeType;
      item.uri = r.uri ?? '';
      item.pixelMap = r.pixelMap !== undefined && r.pixelMap !== null;
      // A record's plain text, or the text of its HTML or URI.
      item.text = r.plainText ? r.plainText : r.toPlainText();
      items.push(item);
    }
    const c: Choice = choose(items);
    const tag = data.getProperty().tag;
    if (c.kind === TEXT) {
      const b = truncateUtf8(utf8.encode(c.text), MAX_TEXT);
      await this.send(tag, TEXT, textKey(b), b);
    } else if (c.kind === IMAGE) {
      const img = await pixels(data.getPrimaryPixelMap());
      await this.send(tag, IMAGE, imageKey(img), encodeImage(img));
    } else if (c.kind === FILES && tag !== TAG) {
      const files = this.readFiles(c.uris, items);
      await this.send(tag, FILES, filesKey(files), encodeFiles(files, utf8));
    }
  }

  // Reads the files of a pasteboard clip within the caps of a file clip.
  private readFiles(uris: string[], items: Item[]): ClipFile[] {
    let remaining = MAX_FILE_CLIP - 4;
    return uris.map((uri: string, i: number) => {
      const f = new ClipFile();
      f.name = uriName(uri);
      f.mime = fileMime(items[i].mime, mimeOf(f.name));
      const max = Math.min(MAX_FILE_BYTES, remaining - fileOverhead(f.name, f.mime, utf8));
      f.bytes = readUri(uri, max);
      remaining -= fileOverhead(f.name, f.mime, utf8) + f.bytes.length;
      return f;
    });
  }

  private async send(tag: string, kind: number, key: string, b: Uint8Array): Promise<void> {
    if (!this.echo.outgoing(tag, key)) {
      return;
    }
    const fd = blob(b.buffer.slice(b.byteOffset, b.byteOffset + b.length));
    const reply = await request(this.system, CLIP_SET, encodeHeader(kind, b.length), [fd]);
    close(reply);
    if (reply.status !== 0) {
      hilog.warn(DOMAIN, LOG_TAG, 'CLIP_SET refused: %{public}d', reply.status);
      return;
    }
    log(`clip to the desktop (kind ${kind}, ${b.length} bytes)`);
  }
}
