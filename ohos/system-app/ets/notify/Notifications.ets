import bundleResourceManager from '@ohos.bundle.bundleResourceManager';
import image from '@ohos.multimedia.image';
import notificationManager from '@ohos.notificationManager';
import notificationSubscribe from '@ohos.notificationSubscribe';
import util from '@ohos.util';
import common from '@ohos.app.ability.common';
import Want from '@ohos.app.ability.Want';
import wantAgent, { WantAgent } from '@ohos.app.ability.wantAgent';
import { BusinessError } from '@ohos.base';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { blob, onEvent, open, Reply, request, waitParameter } from 'libinnkeeper.so';
import {
  Action, ACTION, bodyOf, CLICKED, decodeEvent, DISMISSED, encode, idBody, MAX_ICON, Note, NOTIFY,
  NOTIFY_CLOSE, NOTIFY_EVENT, notifyBody, NoteEvent, percent, REPLY, Rgba, rgba, same, scaleFor,
  SHOW_SCREEN, silentOf, Text, titleOf, urgencyOf
} from './Note';

// host/bridge/src/ipc/proto.rs
const SYSTEM = 3;
// libinnkeeper's event type for a new connection
const CONNECTED = 0;
const SELF = 'dev.sargunv.innkeeper.system';
// Progress updates can come many times a second, so a notification goes to
// the desktop at most twice a second.
const MIN_INTERVAL_MS = 500;
const APP_ICON = 64;
const LARGE_ICON = 128;
const LEVEL_DEFAULT = 3;
const DOMAIN = 0x0000;
const TAG = 'InnkeeperNotify';

class Entry {
  id: number;
  request: notificationManager.NotificationRequest;
  level: number = LEVEL_DEFAULT;
  badge: boolean = true;
  // The last blob that the bridge took, and when.
  sent: Uint8Array | undefined = undefined;
  sentAt: number = 0;
  timer: number = -1;

  constructor(id: number, request: notificationManager.NotificationRequest) {
    this.id = id;
    this.request = request;
  }
}

function close(reply: Reply): void {
  reply.fds.forEach((fd: number) => fileIo.closeSync(fd));
}

function message(e: Object): string {
  const b = e as BusinessError;
  return b.code === undefined ? `${(e as Error).message}` : `${b.code} ${b.message}`;
}

// The text of a notification's content, whichever type it has.
function textOf(c: notificationManager.NotificationContent): Text {
  const t = new Text();
  const basic = c.longText ?? c.multiLine ?? c.picture ?? c.normal ?? c.systemLiveView ?? c.liveView;
  if (basic !== undefined) {
    t.title = basic.title ?? '';
    t.text = basic.text ?? '';
  }
  if (c.longText !== undefined) {
    t.expandedTitle = c.longText.expandedTitle ?? '';
    t.longText = c.longText.longText ?? '';
  } else if (c.multiLine !== undefined) {
    t.expandedTitle = c.multiLine.longTitle ?? '';
    t.lines = c.multiLine.lines ?? [];
  } else if (c.picture !== undefined) {
    t.expandedTitle = c.picture.expandedTitle ?? '';
  }
  return t;
}

// The key of a button's text input, or an empty string. OpenHarmony gives
// every button a user input, which is empty without one.
function inputKey(b: notificationManager.NotificationActionButton): string {
  return b.userInput?.inputKey ?? '';
}

// A download template's percentage, or a system live view's progress.
function progressOf(r: notificationManager.NotificationRequest): number {
  if (r.template?.name === 'downloadTemplate') {
    const value = r.template.data['progressValue'];
    const max = r.template.data['progressMaxValue'];
    return percent(typeof value === 'number' ? value : -1, typeof max === 'number' ? max : 100);
  }
  const p = r.content.systemLiveView?.progress;
  return p === undefined ? -1 : percent(p.currentValue ?? -1, p.maxValue ?? 0);
}

// Reads a PixelMap as RGBA, scaled down to fit in max by max.
async function pixels(pm: image.PixelMap | undefined, max: number): Promise<Rgba | undefined> {
  if (pm === undefined) {
    return undefined;
  }
  try {
    const info = pm.getImageInfoSync();
    const s = scaleFor(info.size.width, info.size.height, max);
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
    hilog.warn(DOMAIN, TAG, 'image: %{public}s', message(e));
    return undefined;
  }
}

// Mirrors OpenHarmony's notifications on the desktop, as Android's
// Notifications.kt does. Innkeeper System subscribes to every app's
// notifications (NOTIFICATION_CONTROLLER) and sends each one to the bridge's
// system service as NOTIFY. Clicks, actions, replies and dismissals come back
// as NOTIFY_EVENTs, and the app triggers the notification's WantAgents or
// removes it. SceneBoard keeps showing the notifications in OpenHarmony's
// window too.
class Notifications {
  private context: common.ServiceExtensionContext;
  private system: number = open(SYSTEM, 'innkeeper-notify');
  private byHash: Map<string, Entry> = new Map();
  private byId: Map<number, Entry> = new Map();
  private nextId: number = 1;
  private labels: Map<string, string> = new Map();
  private icons: Map<string, Rgba | undefined> = new Map();
  private decoder: util.TextDecoder = util.TextDecoder.create('utf-8');
  // Requests go one at a time, in the order of OpenHarmony's callbacks.
  private queue: Promise<void> = Promise.resolve();

  constructor(context: common.ServiceExtensionContext) {
    this.context = context;
  }

  start(): void {
    onEvent(this.system, (type: number, body: ArrayBuffer, fds: number[]) => {
      if (type === CONNECTED) {
        // The bridge closed the notifications of the last connection.
        this.byHash.forEach((e: Entry) => {
          e.sent = undefined;
          this.schedule(e);
        });
      } else if (type === NOTIFY_EVENT) {
        const e = decodeEvent(body);
        if (e !== undefined) {
          const text = e.what === REPLY && fds.length > 0 ? this.read(fds[0], e.len) : '';
          this.enqueue(() => this.onDesktop(e, text));
        }
      }
      fds.forEach((fd: number) => fileIo.closeSync(fd));
    });
    const subscriber: notificationSubscribe.NotificationSubscriber = {
      onConsume: (d: notificationSubscribe.SubscribeCallbackData): void =>
        this.posted(d.request, d.sortingMap?.sortings[d.request.hashCode ?? '']?.slot),
      onCancel: (d: notificationSubscribe.SubscribeCallbackData): void => this.removed(d.request),
    };
    notificationSubscribe.subscribe(subscriber).then(() => {
      hilog.info(DOMAIN, TAG, 'subscribed');
      return notificationManager.getAllActiveNotifications();
    }).then((active: notificationManager.NotificationRequest[]) => {
      active.forEach((r: notificationManager.NotificationRequest) => this.posted(r, undefined));
    }).catch((e: Object) => {
      hilog.error(DOMAIN, TAG, 'subscribing: %{public}s', message(e));
    });
  }

  private enqueue(job: () => Promise<void>): void {
    this.queue = this.queue.then(job).catch((e: Object) => {
      hilog.error(DOMAIN, TAG, 'notifications: %{public}s', message(e));
    });
  }

  private read(fd: number, len: number): string {
    const buf = new ArrayBuffer(Math.min(len, 4096));
    const n = fileIo.readSync(fd, buf, { offset: 0 });
    return this.decoder.decodeToString(new Uint8Array(buf, 0, n));
  }

  // `slot` is the notification's slot, which OpenHarmony passes with a new
  // notification.
  private posted(r: notificationManager.NotificationRequest,
    slot: notificationManager.NotificationSlot | undefined): void {
    const hash = r.hashCode ?? '';
    const bundle = r.creatorBundleName ?? '';
    if (hash.length === 0 || bundle.length === 0 || bundle === SELF) {
      return;
    }
    let e = this.byHash.get(hash);
    if (e === undefined) {
      e = new Entry(this.nextId++, r);
      this.byHash.set(hash, e);
      this.byId.set(e.id, e);
    }
    e.request = r;
    if (slot !== undefined) {
      e.level = slot.notificationLevel ?? slot.level ?? LEVEL_DEFAULT;
      e.badge = slot.badgeFlag ?? true;
    }
    this.schedule(e);
  }

  private removed(r: notificationManager.NotificationRequest): void {
    const e = this.byHash.get(r.hashCode ?? '');
    if (e === undefined) {
      return;
    }
    this.byHash.delete(r.hashCode ?? '');
    this.byId.delete(e.id);
    clearTimeout(e.timer);
    if (e.sent !== undefined) {
      e.sent = undefined;
      this.enqueue(async (): Promise<void> => {
        close(await request(this.system, NOTIFY_CLOSE, idBody(e.id), []));
      });
    }
  }

  // Sends `e` now, or once its interval has passed.
  private schedule(e: Entry): void {
    clearTimeout(e.timer);
    const wait = e.sentAt + MIN_INTERVAL_MS - Date.now();
    if (wait > 0) {
      e.timer = setTimeout(() => this.enqueue(() => this.send(e)), wait);
    } else {
      this.enqueue(() => this.send(e));
    }
  }

  private async send(e: Entry): Promise<void> {
    if (this.byId.get(e.id) !== e) {
      return;
    }
    const b = encode(await this.note(e));
    if (same(e.sent, b)) {
      return;
    }
    const reply = await request(this.system, NOTIFY, notifyBody(e.id, b.length), [blob(b.buffer)]);
    close(reply);
    if (reply.status !== 0) {
      hilog.warn(DOMAIN, TAG, 'NOTIFY %{public}d (%{public}s): %{public}d', e.id,
        e.request.creatorBundleName, reply.status);
      return;
    }
    hilog.info(DOMAIN, TAG, 'notification %{public}d (%{public}s) sent', e.id, e.request.creatorBundleName);
    e.sent = b;
    e.sentAt = Date.now();
  }

  private async note(e: Entry): Promise<Note> {
    const r = e.request;
    const n = new Note();
    n.bundle = r.creatorBundleName ?? '';
    n.appName = this.label(n.bundle);
    const t = textOf(r.content);
    n.title = titleOf(t);
    n.body = bodyOf(t);
    n.ongoing = r.isOngoing === true;
    n.silent = silentOf(e.level, e.sent !== undefined, r.isAlertOnce === true);
    n.clickable = r.wantAgent !== undefined;
    n.badge = e.badge;
    n.urgency = urgencyOf(e.level, r.notificationSlotType ?? -1);
    n.progress = progressOf(r);
    n.actions = (r.actionButtons ?? []).map((b: notificationManager.NotificationActionButton) => {
      const a = new Action();
      a.label = b.title;
      a.reply = inputKey(b).length > 0;
      return a;
    });
    n.icon = await this.icon(n.bundle, r.smallIcon);
    const picture = r.content.picture?.picture;
    n.picture = picture !== undefined ? await pixels(picture, MAX_ICON) : await pixels(r.largeIcon, LARGE_ICON);
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

  // The app's icon from its bundle resources, or else the notification's
  // small icon. The bridge shows it when the desktop has no entry for the app.
  private async icon(bundle: string, small: image.PixelMap | undefined): Promise<Rgba | undefined> {
    if (!this.icons.has(bundle)) {
      let icon: Rgba | undefined = undefined;
      try {
        const info = bundleResourceManager.getBundleResourceInfo(bundle,
          bundleResourceManager.ResourceFlag.GET_RESOURCE_INFO_WITH_ICON);
        const png = new util.Base64Helper().decodeSync(info.icon.replace(/^data:[^,]*,/, ''));
        const source = image.createImageSource(png.buffer);
        try {
          icon = await pixels(await source.createPixelMap(), APP_ICON);
        } finally {
          await source.release();
        }
      } catch (e) {
        hilog.warn(DOMAIN, TAG, '%{public}s: no icon: %{public}s', bundle, message(e));
      }
      this.icons.set(bundle, icon);
    }
    const icon = this.icons.get(bundle);
    return icon !== undefined ? icon : await pixels(small, APP_ICON);
  }

  private async onDesktop(ev: NoteEvent, text: string): Promise<void> {
    const e = this.byId.get(ev.id);
    if (e === undefined) {
      return;
    }
    const r = e.request;
    hilog.info(DOMAIN, TAG, 'desktop: notification %{public}d (%{public}s) event %{public}d action %{public}d',
      e.id, r.creatorBundleName, ev.what, ev.action);
    const button = r.actionButtons?.[ev.action];
    if (ev.what === CLICKED) {
      if (r.wantAgent !== undefined) {
        await this.trigger(e, r.wantAgent, undefined);
      }
      if (r.tapDismissed !== false) {
        await this.remove(r, notificationSubscribe.RemoveReason.CLICK_REASON_REMOVE);
      }
    } else if (ev.what === ACTION && button !== undefined) {
      await this.trigger(e, button.wantAgent, undefined);
    } else if (ev.what === REPLY && button !== undefined && inputKey(button).length > 0) {
      const input: Record<string, Object> = {};
      input[inputKey(button)] = text;
      await this.trigger(e, button.wantAgent, input);
    } else if (ev.what === DISMISSED && r.isOngoing !== true && r.isUnremovable !== true) {
      await this.remove(r, notificationSubscribe.RemoveReason.CANCEL_REASON_REMOVE);
    }
  }

  // Runs a WantAgent, with `input` added to its want's parameters for a
  // reply.
  //
  // AMS lets a WantAgent start an ability only when the process that
  // triggers it is in the foreground, as SceneBoard is, or when the app that
  // made the agent may start abilities from the background. This app runs in
  // the background, so it starts the agent's ability itself with its own
  // permissions. The ability opens on OpenHarmony's screen, so the bridge
  // shows the screen's window first. Other agents, such as common events,
  // trigger as they are.
  private async trigger(e: Entry, agent: WantAgent, input: Record<string, Object> | undefined):
    Promise<void> {
    const op = await wantAgent.getOperationType(agent);
    if (op === wantAgent.OperationType.START_ABILITY || op === wantAgent.OperationType.START_ABILITIES) {
      const want = await wantAgent.getWant(agent);
      if (input !== undefined) {
        const params: Record<string, Object> = want.parameters ?? {};
        Object.keys(input).forEach((k: string) => {
          params[k] = input[k];
        });
        want.parameters = params;
      }
      close(await request(this.system, SHOW_SCREEN, idBody(e.id), []));
      try {
        await this.context.startAbility(want);
      } catch (err) {
        hilog.warn(DOMAIN, TAG, 'starting %{public}s: %{public}s', want.abilityName, message(err));
      }
      return;
    }
    const info: wantAgent.TriggerInfo = { code: 0 };
    if (input !== undefined) {
      const want: Want = { parameters: input };
      info.want = want;
    }
    wantAgent.trigger(agent, info, (err: BusinessError, data: wantAgent.CompleteData) => {
      if (err?.code) {
        hilog.warn(DOMAIN, TAG, 'trigger %{public}d: %{public}s', e.id, message(err));
      } else if (data?.finalCode) {
        hilog.warn(DOMAIN, TAG, 'trigger %{public}d: code %{public}d', e.id, data.finalCode);
      }
    });
  }

  private async remove(r: notificationManager.NotificationRequest,
    reason: notificationSubscribe.RemoveReason): Promise<void> {
    try {
      await notificationSubscribe.remove(r.hashCode ?? '', reason);
    } catch (e) {
      hilog.warn(DOMAIN, TAG, 'remove: %{public}s', message(e));
    }
  }
}

// Starts mirroring once boot completes and the notification service is up.
export function startNotifications(context: common.ServiceExtensionContext): void {
  waitParameter('bootevent.boot.completed', 'true').then(() => {
    new Notifications(context).start();
  }).catch((e: Error) => {
    hilog.error(DOMAIN, TAG, 'waiting for boot: %{public}s', e.message);
  });
}
