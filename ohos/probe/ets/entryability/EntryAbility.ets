import { AbilityConstant, UIAbility, Want, wantAgent, WantAgent } from '@kit.AbilityKit';
import { notificationManager } from '@kit.NotificationKit';
import { audio } from '@kit.AudioKit';
import { hilog } from '@kit.PerformanceAnalysisKit';
import { window } from '@kit.ArkUI';

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

// The tests read the URI of each want from hilog. `aa start --ps mode
// capture` records from the microphone, and `--ps mode notify` posts a
// notification.
function run(want: Want): void {
  if (want.uri) {
    console.info(`innkeeper-probes: opened ${want.uri}`);
  }
  const mode = want.parameters?.['mode'];
  if (mode === 'capture') {
    capture();
  } else if (mode === 'notify') {
    notify();
  } else if (mode === 'clicked' || mode === 'action' || mode === 'reply') {
    hilog.info(0x0000, TAG, 'notification %{public}s: %{public}s', mode, `${want.parameters?.['reply'] ?? ''}`);
  }
}

export default class EntryAbility extends UIAbility {
  onCreate(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    run(want);
  }

  onNewWant(want: Want, launchParam: AbilityConstant.LaunchParam): void {
    run(want);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    try {
      windowStage.loadContent('pages/Index');
    } catch (e) {
      console.error(`loadContent failed: ${JSON.stringify(e)}`);
    }
  }
}
