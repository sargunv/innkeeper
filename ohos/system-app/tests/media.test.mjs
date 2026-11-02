// Host tests for ets/media/Media.ets.
import assert from "node:assert/strict";
import { test } from "node:test";

import { load } from "./ets.mjs";

const media = await load("Media", "media");

function playback(fields) {
  return Object.assign(new media.Playback(), fields);
}

function metadata(fields) {
  return Object.assign(new media.Metadata(), fields);
}

// avSession.PlaybackState values.
const PREPARE = 1;
const PLAY = 2;
const PAUSE = 3;
const STOP = 6;
const COMPLETED = 7;
const BUFFERING = 11;

test("playing, buffering and seeking states play, and preparing pauses", () => {
  assert.equal(media.stateOf(PLAY), media.PLAYING);
  assert.equal(media.stateOf(4), media.PLAYING);
  assert.equal(media.stateOf(5), media.PLAYING);
  assert.equal(media.stateOf(BUFFERING), media.PLAYING);
  assert.equal(media.stateOf(PAUSE), media.PAUSED);
  assert.equal(media.stateOf(PREPARE), media.PAUSED);
  assert.equal(media.stateOf(STOP), media.STOPPED);
  assert.equal(media.stateOf(COMPLETED), media.STOPPED);
  assert.equal(media.stateOf(undefined), media.STOPPED);
});

test("valid commands become the bridge's action bits", () => {
  assert.equal(media.actionsOf([]), 0);
  assert.equal(
    media.actionsOf([
      "play",
      "pause",
      "playNext",
      "playPrevious",
      "seek",
      "stop",
    ]),
    63,
  );
  assert.equal(media.actionsOf(["pause", "setSpeed", "toggleFavorite"]), 2);
});

test("a playing session's position moves on at its speed", () => {
  const p = playback({
    state: PLAY,
    speed: 2,
    elapsedTime: 1000,
    updateTime: 10_000,
  });
  assert.equal(media.positionOf(p, 10_500), 2_000_000);
  assert.equal(media.positionOf({ ...p, state: PAUSE }, 10_500), 1_000_000);
  assert.equal(media.positionOf(playback({ state: PLAY }), 10_500), -1);
  assert.equal(media.positionOf(playback({ elapsedTime: -1 }), 0), -1);
});

test("metadata fills in the media, with the author as a fallback artist", () => {
  const m = media.mediaOf(
    "org.example.player",
    "Player",
    metadata({
      title: "Song",
      author: "Author",
      subtitle: "Sub",
      album: "Album",
      duration: 180_000,
    }),
    playback({ state: PLAY, elapsedTime: 0, updateTime: 5 }),
    ["play", "pause"],
    5,
  );
  assert.deepEqual(
    {
      title: m.title,
      artist: m.artist,
      album: m.album,
      state: m.state,
      actions: m.actions,
      durationUs: m.durationUs,
      positionUs: m.positionUs,
      rate: m.rate,
    },
    {
      title: "Song",
      artist: "Author",
      album: "Album",
      state: media.PLAYING,
      actions: 3,
      durationUs: 180_000_000,
      positionUs: 0,
      rate: 1,
    },
  );
  const bare = media.mediaOf(
    "b",
    "B",
    metadata({ duration: 0 }),
    playback({ speed: NaN }),
    [],
    0,
  );
  assert.equal(bare.durationUs, -1);
  assert.equal(bare.positionUs, -1);
  assert.equal(bare.rate, 1);
  assert.equal(
    media.mediaOf("b", "B", metadata({ duration: 2e10 }), playback({}), [], 0)
      .durationUs,
    -1,
  );
});

test("the blob has the bridge's layout", () => {
  const m = new media.Media();
  m.bundle = "a.b";
  m.appName = "A";
  m.title = "T";
  m.state = media.PAUSED;
  m.actions = 3;
  m.durationUs = 0x1_0000_0002;
  m.positionUs = -1;
  m.rate = 1.5;
  const b = media.encode(m);
  const v = new DataView(b.buffer);
  let at = 0;
  const u32 = () => {
    at += 4;
    return v.getUint32(at - 4, true);
  };
  const text = () => {
    const n = u32();
    at += n;
    return new TextDecoder().decode(b.subarray(at - n, at));
  };
  assert.equal(u32(), 1);
  assert.deepEqual(
    [text(), text(), text(), text(), text()],
    ["a.b", "A", "T", "", ""],
  );
  assert.deepEqual([...b.subarray(at, at + 4)], [media.PAUSED, 0, 0, 0]);
  at += 4;
  assert.equal(u32(), 3);
  assert.equal(v.getBigInt64(at, true), 0x1_0000_0002n);
  assert.equal(v.getBigInt64(at + 8, true), -1n);
  assert.equal(v.getFloat32(at + 16, true), 1.5);
  // No artwork is a 0 by 0 image.
  assert.deepEqual([...b.subarray(at + 20)], [0, 0, 0, 0]);
});

test("artwork follows the numbers", () => {
  const m = new media.Media();
  const art = new (class {
    width = 1;
    height = 1;
    data = new Uint8Array([1, 2, 3, 4]);
  })();
  m.art = art;
  const b = media.encode(m);
  assert.deepEqual([...b.subarray(b.length - 8)], [1, 0, 1, 0, 1, 2, 3, 4]);
});

test("updates that only move the position on aren't sent, and seeks are", () => {
  const sent = new media.Sent();
  const m = new media.Media();
  m.state = media.PLAYING;
  m.positionUs = 0;
  assert.equal(sent.update(m, "", 0), true);
  // Ten seconds on, the position has moved ten seconds.
  assert.equal(
    sent.update({ ...m, positionUs: 10_000_000 }, "", 10_000),
    false,
  );
  // A jump of a minute is a seek.
  assert.equal(sent.update({ ...m, positionUs: 70_000_000 }, "", 10_000), true);
  assert.equal(
    sent.update({ ...m, positionUs: 70_000_000, title: "New" }, "", 10_000),
    true,
  );
  assert.equal(
    sent.update({ ...m, positionUs: 70_000_000, title: "New" }, "art", 10_000),
    true,
  );
  sent.reset();
  assert.equal(
    sent.update({ ...m, positionUs: 70_000_000, title: "New" }, "art", 10_000),
    true,
  );
});

test("a MEDIA_CMD body decodes, with a negative argument", () => {
  const b = new ArrayBuffer(16);
  const v = new DataView(b);
  v.setUint32(0, 7, true);
  v.setUint8(4, media.SEEK);
  v.setBigInt64(8, -5_000_000n, true);
  assert.deepEqual(
    { ...media.decodeCommand(b) },
    { player: 7, cmd: media.SEEK, arg: -5_000_000 },
  );
  assert.equal(media.decodeCommand(new ArrayBuffer(15)), undefined);
});

function command(cmd, arg = 0) {
  return Object.assign(new media.Command(), { player: 1, cmd, arg });
}

test("desktop commands become control commands", () => {
  const playing = playback({
    state: PLAY,
    speed: 1,
    elapsedTime: 10_000,
    updateTime: 1000,
  });
  const paused = playback({
    state: PAUSE,
    elapsedTime: 10_000,
    updateTime: 1000,
  });
  const control = (c, p = paused, now = 1000) => {
    const r = media.controlFor(c, p, now);
    return r === undefined ? undefined : { ...r };
  };
  assert.deepEqual(control(command(media.PLAY)), {
    command: "play",
    parameter: undefined,
  });
  assert.deepEqual(control(command(media.PAUSE)), {
    command: "pause",
    parameter: undefined,
  });
  assert.deepEqual(control(command(media.PLAY_PAUSE), playing), {
    command: "pause",
    parameter: undefined,
  });
  assert.deepEqual(control(command(media.PLAY_PAUSE), paused), {
    command: "play",
    parameter: undefined,
  });
  assert.equal(control(command(media.STOP)).command, "stop");
  assert.equal(control(command(media.NEXT)).command, "playNext");
  assert.equal(control(command(media.PREVIOUS)).command, "playPrevious");
  // Seeking by 5 s from 10 s, two seconds after the playing session's last
  // update, goes to 17 s.
  assert.deepEqual(control(command(media.SEEK, 5_000_000), playing, 3000), {
    command: "seek",
    parameter: 17_000,
  });
  assert.deepEqual(control(command(media.SEEK, -60_000_000)), {
    command: "seek",
    parameter: 0,
  });
  assert.deepEqual(control(command(media.SET_POSITION, 42_000_000)), {
    command: "seek",
    parameter: 42_000,
  });
  assert.equal(control(command(media.RAISE)), undefined);
  assert.equal(control(command(99)), undefined);
});
