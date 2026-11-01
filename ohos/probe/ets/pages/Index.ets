// The text field is for the text input test, which clicks it and reads its
// text from WindowManagerService's inspector dump.
@Entry
@Component
struct Index {
  build() {
    Column({ space: 24 }) {
      Text('Innkeeper probes').fontSize(32)
      TextInput({ placeholder: 'Text' })
        .id('probe_text')
        .width('90%')
        .height(64)
    }
    .width('100%')
    .height('100%')
    .justifyContent(FlexAlign.Center)
  }
}
