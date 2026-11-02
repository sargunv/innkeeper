import avSession from '@ohos.multimedia.avsession';
import bundleResourceManager from '@ohos.bundle.bundleResourceManager';
import image from '@ohos.multimedia.image';
import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import wantAgent from '@ohos.app.ability.wantAgent';
import { BusinessError } from '@ohos.base';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, onEvent, open, Reply, request, waitParameter } from 'libinnkeeper.so';
import { idBody, rgba, Rgba, scaleFor, SHOW_SCREEN } from '../notify/Note';
import {
  closeBody, Command, controlFor, Control, decodeCommand, encode, Media, MAX_ART, MEDIA, MEDIA_CLOSE, MEDIA_CMD,
  mediaBody, mediaOf, Metadata, Playback, RAISE, Sent
} from './Media';

// host/bridge/src/ipc/proto.rs
const SYSTEM = 3;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const SELF = 'dev.sargunv.innkeeper.system';
// The bridge takes at most this many players from one connection.
const MAX_PLAYERS = 8;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperMedia';

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

function metadataOf(md: avSession.AVMetadata): Metadata {
  const m = new Metadata();
  m.title = md.title;
  m.artist = md.artist;
  m.author = md.author;
  m.subtitle = md.subtitle;
  m.album = md.album;
  m.duration = md.duration;
  return m;
}

function playbackOf(s: avSession.AVPlaybackState): Playback {
  const p = new Playback();
  p.state = s.state;
  p.speed = s.speed;
  p.elapsedTime = s.position?.elapsedTime;
  p.updateTime = s.position?.updateTime;
  return p;
}

// Reads the artwork as RGBA, scaled down to fit the bridge's cap.
async function artOf(pm: image.PixelMap): Promise<Rgba | undefined> {
  try {
    const info = pm.getImageInfoSync();
    const s = scaleFor(info.size.width, info.size.height, MAX_ART);
    const copy = await pm.createScaledPixelMap(s, s);
    try {
      const out = copy.getImageInfoSync();
      const buf = new ArrayBuffer(copy.getPixelBytesNumber());
      await copy.readPixelsToBuffer(buf);
      return rgba(buf, out.size.width, out.size.height, copy.getBytesNumberPerRow(), out.pixelFormat,
        out.alphaType === image.AlphaType.PREMUL);
    } finally {
      await copy.release();
    }
  } catch (e) {
    hilog.warn(DOMAIN, TAG, 'artwork: %{public}s', message(e));
    return undefined;
  }
}

class Player {
  id: number;
  descriptor: avSession.AVSessionDescriptor;
  controller: avSession.AVSessionController;
  sent: Sent = new Sent();
  // Whether the bridge has the player.
  published: boolean = false;
  // The session's metadata and artwork, read again on each metadata event.
  // Each read gives a new PixelMap, so `artId` counts the reads that found
  // artwork.
  metadata: Metadata | undefined = undefined;
  art: Rgba | undefined = undefined;
  artId: number = 0;

  constructor(id: number, descriptor: avSession.AVSessionDescriptor, controller: avSession.AVSessionController) {
    this.id = id;
    this.descriptor = descriptor;
    this.controller = controller;
  }
}

// Mirrors OpenHarmony's media sessions as the desktop's MPRIS players, as
// Android's MediaSync does. AVSession's manager lists the sessions and
// announces new and destroyed ones (MANAGE_MEDIA_RESOURCES). The app makes a
// controller for each one and sends its metadata, playback state and valid
// commands to the bridge's system service as MEDIA. MPRIS calls come back as
// MEDIA_CMD events, which become the controller's control commands.
class HostMedia {
  private context: common.ServiceExtensionContext;
  private system: number = open(SYSTEM, 'innkeeper-media');
  private players: Map<string, Player> = new Map();
  private nextId: number = 1;
  private labels: Map<string, string> = new Map();
  // Requests go one at a time, in the order of AVSession's callbacks.
  private queue: Promise<void> = Promise.resolve();

  constructor(context: common.ServiceExtensionContext) {
    this.context = context;
  }

  start(): void {
    onEvent(this.system, (type: number, body: ArrayBuffer, fds: number[]) => {
      fds.forEach((fd: number) => fileIo.closeSync(fd));
      if (type === CONNECTED) {
        // The bridge removed the players of the last connection.
        this.players.forEach((p: Player) => {
          p.published = false;
          p.sent.reset();
          this.enqueue(() => this.send(p, false));
        });
      } else if (type === MEDIA_CMD) {
        const c = decodeCommand(body);
        if (c !== undefined) {
          this.enqueue(() => this.command(c));
        }
      }
    });
    avSession.on('sessionCreate', (d: avSession.AVSessionDescriptor) => this.enqueue(() => this.add(d)));
    avSession.on('sessionDestroy', (d: avSession.AVSessionDescriptor) => this.enqueue(() => this.remove(d.sessionId)));
    avSession.on('topSessionChange', (d: avSession.AVSessionDescriptor) => this.enqueue(() => this.add(d)));
    avSession.getAllSessionDescriptors().then((all: Readonly<avSession.AVSessionDescriptor>[]) => {
      hilog.info(DOMAIN, TAG, 'watching media sessions, %{public}d now', all.length);
      all.forEach((d: avSession.AVSessionDescriptor) => this.enqueue(() => this.add(d)));
    }).catch((e: Object) => {
      hilog.error(DOMAIN, TAG, 'media sessions: %{public}s', message(e));
    });
  }

  private enqueue(job: () => Promise<void>): void {
    this.queue = this.queue.then(job).catch((e: Object) => {
      hilog.error(DOMAIN, TAG, 'media: %{public}s', message(e));
    });
  }

  private async add(d: avSession.AVSessionDescriptor): Promise<void> {
    const bundle = d.elementName.bundleName;
    if (this.players.has(d.sessionId) || bundle === SELF) {
      return;
    }
    const c = await avSession.createController(d.sessionId);
    const p = new Player(this.nextId++, d, c);
    this.players.set(d.sessionId, p);
    hilog.info(DOMAIN, TAG, 'session %{public}d: %{public}s', p.id, bundle);
    const update = (): void => this.enqueue(() => this.send(p, false));
    c.on('metadataChange', 'all', () => this.enqueue(() => this.send(p, true)));
    c.on('playbackStateChange', 'all', update);
    c.on('validCommandChange', update);
    c.on('activeStateChange', update);
    c.on('sessionDestroy', () => this.enqueue(() => this.remove(d.sessionId)));
    await this.send(p, true);
  }

  private async remove(sessionId: string): Promise<void> {
    const p = this.players.get(sessionId);
    if (p === undefined) {
      return;
    }
    this.players.delete(sessionId);
    try {
      await p.controller.destroy();
    } catch (e) {
      // The session's service may have gone with the session.
    }
    await this.unpublish(p);
    hilog.info(DOMAIN, TAG, 'session %{public}d gone', p.id);
  }

  private async unpublish(p: Player): Promise<void> {
    if (p.published) {
      p.published = false;
      p.sent.reset();
      close(await request(this.system, MEDIA_CLOSE, closeBody(p.id), []));
    }
  }

  // Sends the session's state if the desktop's view of it changed, and reads
  // its metadata again if `metadata` is set. An inactive session leaves the
  // desktop, as Android lists active sessions alone.
  private async send(p: Player, metadata: boolean): Promise<void> {
    if (this.players.get(p.descriptor.sessionId) !== p) {
      return;
    }
    const c = p.controller;
    if (!(await c.isActive())) {
      await this.unpublish(p);
      return;
    }
    if (metadata || p.metadata === undefined) {
      const md = await c.getAVMetadata();
      p.metadata = metadataOf(md);
      // The app sends artwork that the session gives as a PixelMap, and it
      // leaves out artwork given as a URI.
      const img = md.mediaImage;
      p.art = img === undefined || typeof img === 'string' ? undefined : await artOf(img);
      p.artId += p.art === undefined ? 0 : 1;
    }
    const state = await c.getAVPlaybackState();
    const commands = await c.getValidCommands();
    const bundle = p.descriptor.elementName.bundleName;
    const now = Date.now();
    const m: Media = mediaOf(bundle, this.label(bundle), p.metadata, playbackOf(state), commands, now);
    m.art = p.art;
    if (!p.published && this.publishedCount() >= MAX_PLAYERS) {
      return;
    }
    if (!p.sent.update(m, p.art === undefined ? '' : `${p.artId}`, now)) {
      return;
    }
    const b = encode(m);
    const reply = await request(this.system, MEDIA, mediaBody(p.id, b.length), [blob(b.buffer)]);
    close(reply);
    if (reply.status !== 0) {
      hilog.warn(DOMAIN, TAG, 'MEDIA %{public}d (%{public}s): %{public}d', p.id, bundle, reply.status);
      p.sent.reset();
      return;
    }
    if (!p.published) {
      hilog.info(DOMAIN, TAG, 'session %{public}d (%{public}s) published', p.id, bundle);
    }
    p.published = true;
  }

  private publishedCount(): number {
    let n = 0;
    this.players.forEach((p: Player) => {
      n += p.published ? 1 : 0;
    });
    return n;
  }

  private label(bundle: string): string {
    let label = this.labels.get(bundle);
    if (label === undefined) {
      try {
        label = bundleResourceManager.getBundleResourceInfo(bundle,
          bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_LABEL).label;
      } catch (e) {
        label = bundle;
      }
      this.labels.set(bundle, label);
    }
    return label;
  }

  private async command(cmd: Command): Promise<void> {
    const player = Array.from(this.players.values()).find((q: Player) => q.id === cmd.player);
    if (player === undefined) {
      return;
    }
    const id = cmd.player;
    hilog.info(DOMAIN, TAG, 'desktop: session %{public}d command %{public}d', id, cmd.cmd);
    if (cmd.cmd === RAISE) {
      await this.raise(player);
      return;
    }
    const state = await player.controller.getAVPlaybackState();
    const c: Control | undefined = controlFor(cmd, playbackOf(state), Date.now());
    if (c === undefined) {
      return;
    }
    const command: avSession.AVControlCommand = { command: c.command as avSession.AVControlCommandType };
    if (c.parameter !== undefined) {
      command.parameter = c.parameter;
    }
    try {
      await player.controller.sendControlCommand(command);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, '%{public}s to session %{public}d: %{public}s', c.command, id, message(e));
    }
  }

  // Opens the session's app, as the session's launch ability says. AMS
  // lets this app start abilities from the background, and the bridge first
  // shows OpenHarmony's screen window, where the app opens.
  private async raise(p: Player): Promise<void> {
    let want: Want = {
      bundleName: p.descriptor.elementName.bundleName,
      abilityName: p.descriptor.elementName.abilityName,
    };
    try {
      want = await wantAgent.getWant(await p.controller.getLaunchAbility());
    } catch (e) {
      // The session has no launch ability, so its own ability opens.
    }
    close(await request(this.system, SHOW_SCREEN, idBody(0), []));
    try {
      await this.context.startAbility(want);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'raising %{public}s: %{public}s', want.bundleName, message(e));
    }
  }
}

// Starts mirroring once boot completes and AVSession's service is up.
export function startMedia(context: common.ServiceExtensionContext): void {
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    new HostMedia(context).start();
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
