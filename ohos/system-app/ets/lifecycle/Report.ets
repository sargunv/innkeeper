// The LIFECYCLE report (host/bridge/src/system/proto.rs), which tells the
// launcher what keeps the session up (docs/session.md, "Lifecycle"). This
// module uses no OpenHarmony API, so its tests run under Node.

export const LIFECYCLE = 0x0308;

// A running continuous task.
export interface Task {
  bundle: string;
  suspended: boolean;
}

// User apps' running continuous tasks, which keep the session up as
// Android's foreground services do. A suspended task doesn't run.
export function services(tasks: Task[], userApps: Set<string>): number {
  return tasks.filter((t: Task) => !t.suspended && userApps.has(t.bundle)).length;
}

// The message body: u32 windows, u32 services, u64 wakeup and u32 blob
// length, little-endian. OpenHarmony apps have no windows of their own, and
// the bridge adds its screen window. No API lists other apps' reminders, so
// the report has no wakeup.
export function body(services: number): ArrayBuffer {
  const b = new ArrayBuffer(20);
  new DataView(b).setUint32(4, services, true);
  return b;
}
