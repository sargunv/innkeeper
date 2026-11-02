import { Target } from '../files/Targets';

// An app in the chooser, with its label and its icon as a data URI.
export class Choice {
  target: Target = new Target();
  label: string = '';
  icon: string = '';
}

// What the chooser shows: the files' URIs and MIME types, and the apps that
// open them. An error replaces the list.
export class Choices {
  uris: string[] = [];
  types: string[] = [];
  apps: Choice[] = [];
  error: string = '';
}
