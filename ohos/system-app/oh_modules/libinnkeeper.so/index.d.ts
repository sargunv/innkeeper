// What libinnkeeper.so (native/napi.c) exports to ArkTS.

// A bridge reply. `status` is 0 or -errno. The caller owns `fds` and closes
// them with fileIo.closeSync.
export interface Reply {
  status: number;
  body: ArrayBuffer;
  fds: number[];
}

// Opens a connection to a bridge service (docs/bridge-protocol.md) and
// returns its handle. A thread connects in the background, and it connects
// again after the bridge restarts.
export const open: (service: number, name: string) => number;

// Calls `callback` with each event on the connection, and with type 0 and an
// empty body each time the connection comes up. The callback owns `fds`.
export const onEvent: (
  handle: number,
  callback: (type: number, body: ArrayBuffer, fds: number[]) => void,
) => void;

// Sends a request and resolves with its reply. It takes at most one fd and
// closes it once sent. It rejects if the connection is down or closes before
// the reply.
export const request: (
  handle: number,
  type: number,
  body: ArrayBuffer,
  fds: number[],
) => Promise<Reply>;

// Returns a memfd that holds `bytes`, sealed as the bridge requires of a blob.
export const blob: (bytes: ArrayBuffer) => number;

// Resolves once the system parameter `name` has the value `value`.
export const waitParameter: (name: string, value: string) => Promise<void>;
