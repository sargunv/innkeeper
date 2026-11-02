import { Choice, Choices, Picker } from './Choice';

// The chooser's dialog over a dimmed backdrop. A click outside the dialog
// cancels it. ChooserAbility and the app selector load this page with the
// choices and a Picker in its LocalStorage. The e2e tests find each app's row
// by its id, `chooser_<bundle>/<ability>`, and the checkbox by
// `chooser_always`, in WindowManagerService's inspector dump.
@Entry({ useSharedStorage: true })
@Component
struct Chooser {
  @LocalStorageLink('choices') choices: Choices = new Choices();
  @State failure: string = '';
  @State always: boolean = false;

  private picker(): Picker | undefined {
    return this.getUIContext().getSharedLocalStorage()?.get<Picker>('picker');
  }

  private close(): void {
    this.picker()?.close();
  }

  private pick(c: Choice): void {
    this.picker()?.pick(c, this.always).catch((e: Error) => {
      this.failure = e.message;
    });
  }

  build() {
    Stack() {
      Column({ space: 12 }) {
        Text($r('app.string.chooser_label'))
          .fontSize(20)
          .fontWeight(FontWeight.Bold)
          .fontColor($r('sys.color.ohos_id_color_text_primary'))
          .width('100%')
          .textAlign(TextAlign.Center)
        if (this.failure !== '' || this.choices.error !== '') {
          Text(this.failure !== '' ? this.failure : this.choices.error)
            .fontSize(16)
            .fontColor($r('sys.color.ohos_id_color_text_secondary'))
            .width('100%')
            .textAlign(TextAlign.Center)
          Button($r('app.string.chooser_ok'))
            .width('100%')
            .onClick(() => this.close())
        } else {
          List({ space: 4 }) {
            ForEach(this.choices.apps, (c: Choice) => {
              ListItem() {
                Row({ space: 16 }) {
                  Image(c.icon)
                    .width(40)
                    .height(40)
                    .objectFit(ImageFit.Contain)
                  Text(c.label)
                    .fontSize(16)
                    .fontColor($r('sys.color.ohos_id_color_text_primary'))
                    .layoutWeight(1)
                    .maxLines(1)
                    .textOverflow({ overflow: TextOverflow.Ellipsis })
                }
                .width('100%')
                .height(56)
                .padding({ left: 12, right: 12 })
                .borderRadius(12)
                .stateStyles({
                  pressed: { .backgroundColor($r('sys.color.ohos_id_color_click_effect')) },
                  normal: { .backgroundColor(Color.Transparent) },
                })
                .id(`chooser_${c.target.bundleName}/${c.target.abilityName}`)
                .onClick(() => this.pick(c))
              }
            }, (c: Choice) => `${c.target.bundleName}/${c.target.moduleName}/${c.target.abilityName}/${c.appIndex}`)
          }
          .width('100%')
          .constraintSize({ maxHeight: 360 })
          if (this.choices.always) {
            Row({ space: 8 }) {
              Checkbox({ name: 'always' })
                .select(this.always)
                .onChange((on: boolean) => {
                  this.always = on;
                })
                .id('chooser_always')
              Text($r('app.string.chooser_always'))
                .fontSize(16)
                .fontColor($r('sys.color.ohos_id_color_text_primary'))
                .layoutWeight(1)
                .onClick(() => {
                  this.always = !this.always;
                })
            }
            .width('100%')
            .padding({ left: 12, right: 12 })
          }
          Button($r('app.string.chooser_cancel'))
            .width('100%')
            .backgroundColor($r('sys.color.ohos_id_color_button_normal'))
            .fontColor($r('sys.color.ohos_id_color_text_primary_activated'))
            .onClick(() => this.close())
        }
      }
      .width('100%')
      .constraintSize({ maxWidth: 400 })
      .padding(24)
      .borderRadius(24)
      .backgroundColor($r('sys.color.ohos_id_color_dialog_bg'))
      .shadow(ShadowStyle.OUTER_DEFAULT_MD)
      // Clicks in the dialog stay in it.
      .onClick(() => {})
    }
    .width('100%')
    .height('100%')
    .padding(16)
    .backgroundColor('#66000000')
    .onClick(() => this.close())
  }
}
