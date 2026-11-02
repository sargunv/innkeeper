import { media } from '@kit.MediaKit';
import { fileIo } from '@kit.CoreFileKit';
import { hilog } from '@kit.PerformanceAnalysisKit';

const TAG = 'InnkeeperProbes';

// Plays the clip at `path` once on the XComponent's surface. The log
// lines say when the player renders its first frame or fails.
async function play(path: string, surfaceId: string): Promise<void> {
  try {
    const file = fileIo.openSync(path, fileIo.OpenMode.READ_ONLY);
    const player = await media.createAVPlayer();
    player.on('error', (e) => hilog.error(0x0000, TAG, 'video error: %{public}s', JSON.stringify(e)));
    player.on('startRenderFrame', () => hilog.info(0x0000, TAG, 'video frame'));
    player.on('stateChange', async (state: string) => {
      hilog.info(0x0000, TAG, 'video %{public}s', state);
      if (state === 'initialized') {
        player.surfaceId = surfaceId;
        await player.prepare();
      } else if (state === 'prepared') {
        await player.play();
      }
    });
    player.fdSrc = { fd: file.fd };
  } catch (e) {
    hilog.error(0x0000, TAG, 'video failed: %{public}s', JSON.stringify(e));
  }
}

// The text field is for the text input test, which clicks it and reads its
// text from WindowManagerService's inspector dump. `--ps mode video` sets the
// video path, and the page then shows the clip below the field.
@Entry
@Component
struct Index {
  @StorageProp('video') video: string = '';
  private surface: XComponentController = new XComponentController();

  build() {
    Column({ space: 24 }) {
      Text('Innkeeper probes').fontSize(32)
      TextInput({ placeholder: 'Text' })
        .id('probe_text')
        .width('90%')
        .height(64)
      if (this.video) {
        XComponent({ id: 'probe_video', type: XComponentType.SURFACE, controller: this.surface })
          .onLoad(() => play(this.video, this.surface.getXComponentSurfaceId()))
          .width('90%')
          .aspectRatio(4 / 3)
      }
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
  }
}
