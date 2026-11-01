import { AbilityConstant, UIAbility, Want } from '@kit.AbilityKit';
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

// The tests read the URI of each want from hilog, and `aa start --ps mode
// capture` records from the microphone.
function run(want: Want): void {
  if (want.uri) {
    console.info(`innkeeper-probes: opened ${want.uri}`);
  }
  if (want.parameters?.['mode'] === 'capture') {
    capture();
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
