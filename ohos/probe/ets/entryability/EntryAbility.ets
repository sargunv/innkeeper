import { AbilityConstant, common, UIAbility, Want, wantAgent, WantAgent } from '@kit.AbilityKit';
import { avSession } from '@kit.AVSessionKit';
import { backgroundTaskManager } from '@kit.BackgroundTasksKit';
import { notificationManager } from '@kit.NotificationKit';
import { audio } from '@kit.AudioKit';
import { image } from '@kit.ImageKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';
import { fileIo } from '@kit.CoreFileKit';
import { util } from '@kit.ArkTS';

const TAG = 'InnkeeperProbes';

// Records half a second from the microphone and logs the loudest sample.
async function capture(): Promise<void> {
  let peak = 0;
  try {
    const capturer = await audio.createAudioCapturer({
      streamInfo: {
        samplingRate: audio.AudioSamplingRate.SAMPLE_RATE_48000,
        channels: audio.AudioChannel.CHANNEL_1,
        sampleFormat: audio.AudioSampleFormat.SAMPLE_FORMAT_S16LE,
        encodingType: audio.AudioEncodingType.ENCODING_TYPE_RAW
      },
      capturerInfo: { source: audio.SourceType.SOURCE_TYPE_MIC, capturerFlags: 0 }
    });
    capturer.on('readData', (buffer: ArrayBuffer) => {
      const samples = new Int16Array(buffer);
      for (let i = 0; i < samples.length; i++) {
        peak = Math.max(peak, Math.abs(samples[i]));
      }
    });
    await capturer.start();
    await new Promise<void>((resolve) => setTimeout(resolve, 500));
    await capturer.stop();
    await capturer.release();
    hilog.info(0x0000, TAG, 'capture peak=%{public}d', peak);
  } catch (e) {
    hilog.error(0x0000, TAG, 'capture failed: %{public}s', JSON.stringify(e));
  }
}

// A WantAgent that starts this ability with `mode`.
function agent(mode: string, code: number): Promise<WantAgent> {
  return wantAgent.getWantAgent({
    wants: [{ bundleName: 'dev.sargunv.innkeeper.probes', abilityName: 'EntryAbility', parameters: { 'mode': mode } }],
    actionType: wantAgent.OperationType.START_ABILITY,
    requestCode: code,
    wantAgentFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG]
  });
}

// Posts a notification with an action button and a reply button. Each one
// starts this ability, which logs it.
async function notify(): Promise<void> {
  try {
    await notificationManager.publish({
      id: 1,
      content: {
        notificationContentType: notificationManager.ContentType.NOTIFICATION_CONTENT_BASIC_TEXT,
        normal: { title: 'Innkeeper probe', text: 'A notification with actions' }
      },
      wantAgent: await agent('clicked', 1),
      actionButtons: [
        { title: 'Ping', wantAgent: await agent('action', 2) },
        { title: 'Reply', wantAgent: await agent('reply', 3), userInput: { inputKey: 'reply' } }
      ]
    });
    hilog.info(0x0000, TAG, 'notification posted');
  } catch (e) {
    hilog.error(0x0000, TAG, 'notification failed: %{public}s', JSON.stringify(e));
  }
}

// The probe's media session. A session lives as long as the object.
let media: avSession.AVSession | undefined = undefined;

// Sets the media session's playback state and logs it.
async function playing(play: boolean): Promise<void> {
  await media?.setAVPlaybackState({
    state: play ? avSession.PlaybackState.PLAYBACK_STATE_PLAY : avSession.PlaybackState.PLAYBACK_STATE_PAUSE,
    speed: 1,
    position: { elapsedTime: 0, updateTime: Date.now() }
  });
  hilog.info(0x0000, TAG, 'media %{public}s', play ? 'play' : 'pause');
}

// Creates a playing audio session with metadata, 2x2 red artwork, and play
// and pause handlers. Each command that reaches the session logs a line.
async function startMedia(context: common.UIAbilityContext): Promise<void> {
  try {
    if (media === undefined) {
      media = await avSession.createAVSession(context, 'probe', 'audio');
      media.on('play', () => playing(true));
      media.on('pause', () => playing(false));
      const red = new Uint8Array([255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255]);
      const art = await image.createPixelMap(red.buffer, {
        size: { width: 2, height: 2 },
        srcPixelFormat: image.PixelMapFormat.RGBA_8888,
        pixelFormat: image.PixelMapFormat.RGBA_8888
      });
      await media.setAVMetadata({
        assetId: 'probe-1',
        title: 'Probe Song',
        artist: 'Probe Artist',
        album: 'Probe Album',
        duration: 180000,
        mediaImage: art
      });
      await media.activate();
    }
    await playing(true);
    hilog.info(0x0000, TAG, 'media session %{public}s', media.sessionId);
  } catch (e) {
    hilog.error(0x0000, TAG, 'media failed: %{public}s', JSON.stringify(e));
  }
}

// Starts or stops an audio playback continuous task and logs the result.
// The task's notification opens this ability.
async function task(context: common.UIAbilityContext, start: boolean): Promise<void> {
  try {
    if (start) {
      const agent = await wantAgent.getWantAgent({
        wants: [{ bundleName: context.abilityInfo.bundleName, abilityName: context.abilityInfo.name }],
        actionType: wantAgent.OperationType.START_ABILITY,
        requestCode: 0,
        actionFlags: [wantAgent.WantAgentFlags.UPDATE_PRESENT_FLAG]
      });
      await backgroundTaskManager.startBackgroundRunning(context,
        backgroundTaskManager.BackgroundMode.AUDIO_PLAYBACK, agent);
    } else {
      await backgroundTaskManager.stopBackgroundRunning(context);
    }
    hilog.info(0x0000, TAG, 'task %{public}s', start ? 'started' : 'stopped');
  } catch (e) {
    hilog.error(0x0000, TAG, 'task failed: %{public}s', JSON.stringify(e));
  }
}

// Logs the first 256 bytes of the file at a file URI, as text.
function read(uri: string): void {
  try {
    const f = fileIo.openSync(uri, fileIo.OpenMode.READ_ONLY);
    const buf = new ArrayBuffer(256);
    const n = fileIo.readSync(f.fd, buf);
    fileIo.closeSync(f);
    const text = util.TextDecoder.create('utf-8').decodeToString(new Uint8Array(buf, 0, n));
    console.info(`innkeeper-probes: read ${text}`);
  } catch (e) {
    console.error(`innkeeper-probes: read failed: ${JSON.stringify(e)}`);
  }
}

// The tests read the URI of each want from hilog, and the content of a file
// URI. `aa start --ps mode
// capture` records from the microphone, `--ps mode notify` posts a
// notification, `--ps mode media` starts a media session, and `--ps task
// start` or `stop` starts or stops a continuous task.
function run(want: Want, context: common.UIAbilityContext): void {
  if (want.uri) {
    console.info(`innkeeper-probes: opened ${want.uri}`);
    if (want.uri.startsWith('file://')) {
      read(want.uri);
    }
  }
  const mode = want.parameters?.['mode'];
  if (mode === 'capture') {
    capture();
  } else if (mode === 'notify') {
    notify();
  } else if (mode === 'media') {
    startMedia(context);
  } else if (mode === 'clicked' || mode === 'action' || mode === 'reply') {
    hilog.info(0x0000, TAG, 'notification %{public}s: %{public}s', mode, `${want.parameters?.['reply'] ?? ''}`);
  }
  const t = want.parameters?.['task'];
  if (t === 'start' || t === 'stop') {
    task(context, t === 'start');
  }
}

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    run(want, this.context);
  }

  onNewWant(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    run(want, this.context);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      windowStage.loadContent('pages/Index');
    } catch (e) {
      console.error(`loadContent failed: ${JSON.stringify(e)}`);
    }
  }
}
