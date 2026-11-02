import { Target } from '../files/Targets';

// An app in the chooser, with its label and its icon as a data URI. appIndex
// names an app clone, and 0 names the app.
export class Choice {
  target: Target = new Target();
  appIndex: number = 0;
  label: string = '';
  icon: string = '';
}

// What the chooser shows: the files' URIs and MIME types, and the apps that
// open them. An error replaces the list. With `always`, the chooser offers to
// make the chosen app the default for the files' type.
export class Choices {
  uris: string[] = [];
  types: string[] = [];
  apps: Choice[] = [];
  error: string = '';
  always: boolean = false;
}

// What the chooser does with the user's answer. ChooserAbility starts the
// app, and the app selector answers AMS. `pick` opens the files in the app,
// and its rejection's message shows in the chooser. `always` is the state of
// the chooser's checkbox. `close` closes the chooser without a choice.
export interface Picker {
  pick(c: Choice, always: boolean): Promise<void>;
  close(): void;
}
