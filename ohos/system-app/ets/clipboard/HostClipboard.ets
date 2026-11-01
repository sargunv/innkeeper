import common from '@ohos.app.ability.common';
import fileUri from '@ohos.file.fileuri';
import image from '@ohos.multimedia.image';
import pasteboard from '@ohos.pasteboard';
import uniformTypeDescriptor from '@ohos.data.uniformTypeDescriptor';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, Reply, request } from 'libinnkeeper.so';
import {
  BGRA, choose, Choice, ClipFile, ClipHeader, decodeFiles, decodeImage, Echo, encodeFiles, encodeHeader,
  encodeImage, FILES, filesSize, fitScale, hash, IMAGE, Item, MAX_FILE, MAX_SELECTION, MAX_TEXT, Rgba,
  RGBA, TAG, TEXT, toRgba, truncateUtf8, Utf8, validMime, validName,
} from './Clip';

// host/bridge/src/system/proto.rs
const CLIP_SET = 0x0304;
const DOMAIN = 0x0000;
const LOG_TAG = 'InnkeeperClipboard';
const OCTET_STREAM = 'application/octet-stream';

const encoder = new util.TextEncoder();
const decoder = util.TextDecoder.create('utf-8', { fatal: true, ignoreBOM: true });

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

// Reads `len` bytes from the start of a blob's memfd.
function readBlob(fd: number, len: number): Uint8Array {
  const b = new ArrayBuffer(len);
  let at = 0;
  while (at < len) {
    const n = fileIo.readSync(fd, b, { offset: at, length: len - at });
    if (n <= 0) {
      throw new Error(`blob ends at ${at} of ${len} bytes`);
    }
    at += n;
  }
  return new Uint8Array(b);
}

// Reads a whole file, refusing one over `max` bytes.
function readFile(uri: string, max: number): Uint8Array {
  const f = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
  try {
    const size = fileIo.statSync(f.fd).size;
    if (size > max) {
      throw new Error(`${size} bytes is over ${max}`);
    }
    const b = new ArrayBuffer(size);
    let at = 0;
    while (at < size) {
      const n = fileIo.readSync(f.fd, b, { offset: at, length: size - at });
      if (n <= 0) {
        break;
      }
      at += n;
    }
    return new Uint8Array(b, 0, at);
  } finally {
    fileIo.closeSync(f);
  }
}

function extension(name: string): string {
  const dot = name.lastIndexOf('.');
  return dot > 0 ? name.substring(dot) : '';
}

// The MIME type of a file name, from OpenHarmony's uniform type descriptors.
function mimeOf(name: string): string {
  const ext = extension(name);
  if (ext.length === 0) {
    return OCTET_STREAM;
  }
  try {
    const id = uniformTypeDescriptor.getUniformDataTypeByFilenameExtension(ext);
    const mime = uniformTypeDescriptor.getTypeDescriptor(id).mimeTypes.find(validMime);
    return mime ?? OCTET_STREAM;
  } catch (e) {
    return OCTET_STREAM;
  }
}

function nameOf(uri: string): string {
  const name = new fileUri.FileUri(uri).name;
  return validName(name, utf8) ? name : 'clipboard-file';
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

// Decodes an image file to a PixelMap no larger than MAX_SIDE on a side.
function decodeImageFile(uri: string): image.PixelMap {
  const f = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
  try {
    const src = image.createImageSource(f.fd);
    try {
      const info = src.getImageInfoSync();
      const scale = fitScale(info.size.width, info.size.height);
      return src.createPixelMapSync({
        desiredSize: { width: Math.round(info.size.width * scale), height: Math.round(info.size.height * scale) },
        desiredPixelFormat: image.PixelMapFormat.RGBA_8888,
      });
    } finally {
      src.release();
    }
  } finally {
    fileIo.closeSync(f);
  }
}

function isImageFile(uri: string): boolean {
  return mimeOf(uri).startsWith('image/');
}

// Syncs OpenHarmony's pasteboard with the desktop's clipboard, as Android's
// ClipboardSync does. The bridge sends each new desktop clip as a CLIP
// event, and the app sends each new pasteboard clip in CLIP_SET.
//
// The app holds READ_PASTEBOARD, so it reads the pasteboard while other apps
// have focus, as Android's system app reads the clipboard. Reading a file
// clip gives the app a read grant on its URIs.
export class HostClipboard {
  private board: pasteboard.SystemPasteboard = pasteboard.getSystemPasteboard();
  private context: common.Context;
  private system: number;
  private echo: Echo = new Echo();
  private queue: Promise<void> = Promise.resolve();
  // Each desktop file clip gets a new directory, because an app may still
  // hold a URI from an earlier clip.
  private serial: number = 0;

  constructor(context: common.Context, system: number) {
    this.context = context;
    this.system = system;
  }

  watch(): void {
    this.board.on('update', () => this.enqueue(() => this.toDesktop()));
    this.removeFiles(-1);
  }

  // Takes a CLIP event and the blob that came with it.
  fromDesktop(h: ClipHeader, fd: number): void {
    let b: Uint8Array;
    try {
      b = readBlob(fd, h.len);
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
    if (kind === TEXT) {
      const text = utf8.decode(b);
      if (text === undefined) {
        return;
      }
      data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_PLAIN, text);
    } else if (kind === IMAGE) {
      const img = decodeImage(b);
      if (img === undefined) {
        return;
      }
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
      const uris = this.store(files);
      data = pasteboard.createData(pasteboard.MIMETYPE_TEXT_URI, uris[0]);
      uris.slice(1).forEach((u: string) => data.addRecord(pasteboard.MIMETYPE_TEXT_URI, u));
    } else {
      return;
    }
    const p = data.getProperty();
    p.tag = TAG;
    p.shareOption = pasteboard.ShareOption.LOCALDEVICE;
    data.setProperty(p);
    this.echo.incoming();
    await this.board.setData(data);
    log(`clip from the desktop (kind ${kind}, ${b.length} bytes)`);
  }

  // Writes a desktop file clip to the app's files directory and returns the
  // files' URIs. The pasteboard grants the pasting app read access to them.
  private store(files: ClipFile[]): string[] {
    const serial = ++this.serial;
    this.removeFiles(serial);
    const dir = `${this.root()}/${serial}`;
    const uris: string[] = [];
    files.forEach((f: ClipFile, i: number) => {
      // A directory per file keeps two files with one name apart.
      fileIo.mkdirSync(`${dir}/${i}`, true);
      const path = `${dir}/${i}/${f.name}`;
      const out = fileIo.openSync(path, fileIo.OpenMode.CREATE | fileIo.OpenMode.WRITE_ONLY | fileIo.OpenMode.TRUNC);
      try {
        fileIo.writeSync(out.fd, f.bytes.buffer.slice(f.bytes.byteOffset, f.bytes.byteOffset + f.bytes.length));
      } finally {
        fileIo.closeSync(out);
      }
      uris.push(fileUri.getUriFromPath(path));
    });
    return uris;
  }

  private root(): string {
    return `${this.context.filesDir}/clipboard`;
  }

  // Removes each earlier clip's files except `keep`'s.
  private removeFiles(keep: number): void {
    try {
      if (!fileIo.accessSync(this.root())) {
        return;
      }
      for (const name of fileIo.listFileSync(this.root())) {
        if (name !== `${keep}`) {
          fileIo.rmdirSync(`${this.root()}/${name}`);
        }
      }
    } catch (e) {
      failed('removing old clipboard files', e as Error);
    }
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
    const c: Choice = choose(items, isImageFile);
    const tag = data.getProperty().tag;
    if (c.kind === TEXT) {
      const b = truncateUtf8(utf8.encode(c.text), MAX_TEXT);
      await this.send(tag, TEXT, `text ${hash(b)}`, b);
    } else if (c.kind === IMAGE) {
      const pm = c.pixelMap ? data.getPrimaryPixelMap() : decodeImageFile(c.uris[0]);
      const img = await pixels(pm);
      await this.send(tag, IMAGE, `image ${img.width}x${img.height} ${hash(img.pixels)}`, encodeImage(img));
    } else if (c.kind === FILES) {
      const files = this.readFiles(c.uris);
      const b = encodeFiles(files, utf8);
      await this.send(tag, FILES, `files ${c.uris.join(' ')} ${hash(b)}`, b);
    }
  }

  private readFiles(uris: string[]): ClipFile[] {
    const files: ClipFile[] = [];
    // The count's four bytes, and then each file's metadata and bytes.
    let left = MAX_SELECTION - 4;
    for (const uri of uris) {
      const f = new ClipFile();
      f.name = nameOf(uri);
      f.mime = mimeOf(f.name);
      left -= filesSize([f], utf8) - 4;
      f.bytes = readFile(uri, Math.min(MAX_FILE, left));
      left -= f.bytes.length;
      files.push(f);
    }
    return files;
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
