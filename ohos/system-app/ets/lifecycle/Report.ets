// The LIFECYCLE report (host/bridge/src/system/proto.rs), which tells the
// launcher what keeps the session up (docs/session.md, "Lifecycle"). This
// module uses no OpenHarmony API, so its tests run under Node.

export const LIFECYCLE = 0x0308;
// The bridge lists at most this many background apps.
export const MAX_BACKGROUND = 32;

// A running continuous task.
export interface Task {
  bundle: string;
  suspended: boolean;
}

// An app that the user lets run in the background.
export interface BackgroundApp {
  bundle: string;
  label: string;
}

// User apps' running continuous tasks, which keep the session up as
// Android's foreground services do. A suspended task doesn't run.
export function services(tasks: Task[], userApps: Set<string>): number {
  return tasks.filter((t: Task) => !t.suspended && userApps.has(t.bundle)).length;
}

// The blob's text, with one `bundle<TAB>label` line for each of the first
// MAX_BACKGROUND apps. Control characters in a label become spaces.
export function backgroundText(apps: BackgroundApp[]): string {
  return apps.slice(0, MAX_BACKGROUND)
    .map((a: BackgroundApp): string => `${a.bundle}\t${a.label.replace(/[\u0000-\u001f\u007f]/g, ' ')}\n`)
    .join('');
}

// The message body: u32 windows, u32 services, u64 wakeup and u32 blob
// length, little-endian. OpenHarmony apps have no windows of their own, and
// the bridge adds its screen window. No API lists other apps' reminders, so
// the report has no wakeup.
export function body(services: number, blobLength: number): ArrayBuffer {
  const b = new ArrayBuffer(20);
  const v = new DataView(b);
  v.setUint32(4, services, true);
  v.setUint32(16, blobLength, true);
  return b;
}
