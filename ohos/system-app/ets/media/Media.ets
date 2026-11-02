// OpenHarmony media sessions as the bridge's media blobs
// (host/bridge/src/system/mpris.rs, `Media`), and the desktop's MPRIS
// commands as AVSession control commands. This module imports no
// OpenHarmony API, so Node runs its tests (tests/media.test.mjs).
// HostMedia.ets reads the sessions and fills in a Media.

import { Rgba, utf8 } from '../notify/Note';

// host/bridge/src/system/proto.rs
export const MEDIA = 0x0306;
export const MEDIA_CLOSE = 0x0307;
export const MEDIA_CMD = 0x4304;

// MEDIA_CMD commands.
export const PLAY = 1;
export const PAUSE = 2;
export const PLAY_PAUSE = 3;
export const STOP = 4;
export const NEXT = 5;
export const PREVIOUS = 6;
// Seek by the argument, in microseconds.
export const SEEK = 7;
// Seek to the argument, in microseconds.
export const SET_POSITION = 8;
export const RAISE = 9;

// The blob's playback states.
export const STOPPED = 0;
export const PAUSED = 1;
export const PLAYING = 2;

// The blob's action bits.
const ACTION_PLAY = 1;
const ACTION_PAUSE = 2;
const ACTION_NEXT = 4;
const ACTION_PREVIOUS = 8;
const ACTION_SEEK = 16;
const ACTION_STOP = 32;

// The bridge's caps.
export const MAX_ART = 512;
// The longest duration or position that the bridge takes, about four months.
const MAX_US = 1e13;
const MAX_RATE = 16;

// avSession.PlaybackState values.
const STATE_PREPARE = 1;
const STATE_PLAY = 2;
const STATE_PAUSE = 3;
const STATE_FAST_FORWARD = 4;
const STATE_REWIND = 5;
const STATE_BUFFERING = 11;

// A position change this far from the bridge's estimate is a seek, which the
// bridge has to hear about.
const SEEK_US = 1500000;

export class Media {
  bundle: string = '';
  appName: string = '';
  title: string = '';
  artist: string = '';
  album: string = '';
  state: number = STOPPED;
  actions: number = 0;
  // In microseconds, or -1 if unknown.
  durationUs: number = -1;
  positionUs: number = -1;
  rate: number = 1;
  art: Rgba | undefined = undefined;
}

// The parts of an AVMetadata that the desktop shows.
export class Metadata {
  title?: string;
  artist?: string;
  author?: string;
  subtitle?: string;
  album?: string;
  // In milliseconds.
  duration?: number;
}

// The parts of an AVPlaybackState that the desktop shows.
export class Playback {
  state?: number;
  speed?: number;
  // In milliseconds, as OpenHarmony's PlaybackPosition. `updateTime` is the
  // wall-clock time at which `elapsedTime` was current.
  elapsedTime?: number;
  updateTime?: number;
}

// The blob's state for an OpenHarmony playback state. Buffering, seeking
// and fast-forward count as playing, as Android's MediaSync counts them, and
// a session that prepares counts as paused.
export function stateOf(s: number | undefined): number {
  switch (s) {
    case STATE_PLAY:
    case STATE_FAST_FORWARD:
    case STATE_REWIND:
    case STATE_BUFFERING:
      return PLAYING;
    case STATE_PAUSE:
    case STATE_PREPARE:
      return PAUSED;
    default:
      return STOPPED;
  }
}

// The blob's action bits for a session's valid commands.
export function actionsOf(commands: string[]): number {
  let a = 0;
  commands.forEach((c: string) => {
    if (c === 'play') {
      a |= ACTION_PLAY;
    } else if (c === 'pause') {
      a |= ACTION_PAUSE;
    } else if (c === 'playNext') {
      a |= ACTION_NEXT;
    } else if (c === 'playPrevious') {
      a |= ACTION_PREVIOUS;
    } else if (c === 'seek') {
      a |= ACTION_SEEK;
    } else if (c === 'stop') {
      a |= ACTION_STOP;
    }
  });
  return a;
}

function speedOf(p: Playback): number {
  const s = p.speed;
  return s !== undefined && Number.isFinite(s) && s > 0 && s <= MAX_RATE ? s : 1;
}

// Milliseconds as the bridge's microseconds, or -1 when unknown or out of
// the bridge's range.
function us(ms: number): number {
  const v = Math.round(ms * 1000);
  return Number.isFinite(v) && v >= 0 && v <= MAX_US ? v : -1;
}

// Where the session is at `now`, in microseconds, or -1. A playing
// session's position moves on from its last update at its speed.
export function positionOf(p: Playback, now: number): number {
  const elapsed = p.elapsedTime;
  if (elapsed === undefined || !(elapsed >= 0)) {
    return -1;
  }
  let ms = elapsed;
  const at = p.updateTime ?? 0;
  if (stateOf(p.state) === PLAYING && at > 0 && now > at) {
    ms += (now - at) * speedOf(p);
  }
  return us(ms);
}

// Fills in a Media from a session's metadata, playback state and commands.
// The artist falls back to the author and then the subtitle, as Android's
// MediaSync falls back to the album artist and then the subtitle.
export function mediaOf(bundle: string, appName: string, md: Metadata, p: Playback, commands: string[],
  now: number): Media {
  const m = new Media();
  m.bundle = bundle;
  m.appName = appName;
  m.title = md.title ?? '';
  m.artist = md.artist || md.author || md.subtitle || '';
  m.album = md.album ?? '';
  m.state = stateOf(p.state);
  m.actions = actionsOf(commands);
  m.durationUs = md.duration !== undefined && md.duration > 0 ? us(md.duration) : -1;
  m.positionUs = positionOf(p, now);
  m.rate = speedOf(p);
  return m;
}

// What the bridge last took for a player, to leave out updates that only
// move the position on, which the bridge extrapolates.
export class Sent {
  key: string = '';
  positionUs: number = -1;
  at: number = 0;
  playing: boolean = false;
  rate: number = 1;

  // Where the bridge thinks the player is at `now`, or -1.
  expected(now: number): number {
    if (this.positionUs < 0) {
      return -1;
    }
    return this.playing ? this.positionUs + (now - this.at) * 1000 * this.rate : this.positionUs;
  }

  // Whether `m` changes what the bridge shows. `art` identifies the artwork.
  // If so, the tracker takes `m` as sent.
  update(m: Media, art: string, now: number): boolean {
    const key = JSON.stringify([m.title, m.artist, m.album, m.state, m.actions, m.durationUs, m.rate, art]);
    const expected = this.expected(now);
    const seeked = m.positionUs >= 0 && expected >= 0 && Math.abs(m.positionUs - expected) > SEEK_US;
    if (key === this.key && !seeked) {
      return false;
    }
    this.key = key;
    this.positionUs = m.positionUs;
    this.at = now;
    this.playing = m.state === PLAYING;
    this.rate = m.rate;
    return true;
  }

  // Makes the next update send, as after the bridge reconnects.
  reset(): void {
    this.key = '';
  }
}

// Little-endian writer for the blob.
class Writer {
  bytes: number[] = [];

  u8(v: number): Writer {
    this.bytes.push(v & 0xff);
    return this;
  }

  u16(v: number): Writer {
    return this.u8(v).u8(v >>> 8);
  }

  u32(v: number): Writer {
    return this.u16(v & 0xffff).u16(v >>> 16);
  }

  // A whole number from -1 to 2^53 as a two's complement i64.
  i64(v: number): Writer {
    if (v < 0) {
      return this.u32(0xffffffff).u32(0xffffffff);
    }
    return this.u32(v % 0x100000000).u32(Math.floor(v / 0x100000000));
  }

  f32(v: number): Writer {
    const b = new Uint8Array(4);
    new DataView(b.buffer).setFloat32(0, v, true);
    b.forEach((x: number) => {
      this.bytes.push(x);
    });
    return this;
  }

  // A u32 length and at most `max` bytes of UTF-8.
  text(s: string, max: number): Writer {
    const b = utf8(s, max);
    this.u32(b.length);
    b.forEach((x: number) => {
      this.bytes.push(x);
    });
    return this;
  }

  image(img: Rgba | undefined): Writer {
    if (img === undefined) {
      return this.u16(0).u16(0);
    }
    this.u16(img.width).u16(img.height);
    img.data.forEach((x: number) => {
      this.bytes.push(x);
    });
    return this;
  }
}

// The media blob: u32 version (1), text bundle, app name, title, artist and
// album, u8 state, u8 0, u16 0, u32 actions, i64 duration and position in
// microseconds, f32 rate, and the artwork. Text is cut to the bridge's caps.
export function encode(m: Media): Uint8Array {
  const w = new Writer()
    .u32(1)
    .text(m.bundle, 255)
    .text(m.appName, 256)
    .text(m.title, 1024)
    .text(m.artist, 1024)
    .text(m.album, 1024)
    .u8(m.state)
    .u8(0)
    .u16(0)
    .u32(m.actions)
    .i64(m.durationUs)
    .i64(m.positionUs)
    .f32(m.rate);
  w.image(m.art);
  return new Uint8Array(w.bytes);
}

// MEDIA's body: u32 player, u32 blob length.
export function mediaBody(player: number, len: number): ArrayBuffer {
  return new Uint8Array(new Writer().u32(player).u32(len).bytes).buffer;
}

// MEDIA_CLOSE's body: u32 player.
export function closeBody(player: number): ArrayBuffer {
  return new Uint8Array(new Writer().u32(player).bytes).buffer;
}

export class Command {
  player: number = 0;
  cmd: number = 0;
  // In microseconds.
  arg: number = 0;
}

// Reads a MEDIA_CMD body: u32 player, u8 cmd, u8 0, u16 0, i64 arg.
export function decodeCommand(body: ArrayBuffer): Command | undefined {
  if (body.byteLength !== 16) {
    return undefined;
  }
  const v = new DataView(body);
  const c = new Command();
  c.player = v.getUint32(0, true);
  c.cmd = v.getUint8(4);
  c.arg = v.getInt32(12, true) * 0x100000000 + v.getUint32(8, true);
  return c;
}

// An AVSession control command.
export class Control {
  command: string = '';
  // For seek, the position in milliseconds.
  parameter: number | undefined = undefined;
}

function control(command: string, parameter?: number): Control {
  const c = new Control();
  c.command = command;
  c.parameter = parameter;
  return c;
}

// The control command for a desktop command, given the session's playback
// state at `now`. Play/pause pauses a playing session and plays any other.
// Returns undefined for raise, which isn't a control command, and for
// commands that the bridge doesn't send.
export function controlFor(c: Command, p: Playback, now: number): Control | undefined {
  switch (c.cmd) {
    case PLAY:
      return control('play');
    case PAUSE:
      return control('pause');
    case PLAY_PAUSE:
      return control(stateOf(p.state) === PLAYING ? 'pause' : 'play');
    case STOP:
      return control('stop');
    case NEXT:
      return control('playNext');
    case PREVIOUS:
      return control('playPrevious');
    case SEEK: {
      const at = Math.max(positionOf(p, now), 0);
      return control('seek', Math.max(0, Math.round((at + c.arg) / 1000)));
    }
    case SET_POSITION:
      return control('seek', Math.max(0, Math.round(c.arg / 1000)));
    default:
      return undefined;
  }
}
