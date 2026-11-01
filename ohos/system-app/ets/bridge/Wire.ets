import { util } from '@kit.ArkTS';

const encoder = new util.TextEncoder();

// The bytes of `s` as UTF-8. OpenHarmony's encoder returns nothing for an
// empty string.
export function utf8(s: string): Uint8Array {
  return s.length === 0 ? new Uint8Array(0) : encoder.encodeInto(s);
}

// Builds a message body in the bridge's wire format (docs/bridge-protocol.md):
// little-endian integers, and names as a u16 length and UTF-8.
export class Message {
  private bytes: number[] = [];

  u8(v: number): Message {
    this.bytes.push(v & 0xff);
    return this;
  }

  u16(v: number): Message {
    return this.u8(v).u8(v >>> 8);
  }

  // Also writes an i32, because the bridge reads the same four bytes.
  u32(v: number): Message {
    return this.u16(v & 0xffff).u16(v >>> 16);
  }

  raw(b: Uint8Array): Message {
    b.forEach((x: number) => {
      this.bytes.push(x);
    });
    return this;
  }

  name(s: string): Message {
    const b = utf8(s);
    return this.u16(b.length).raw(b);
  }

  buffer(): ArrayBuffer {
    return new Uint8Array(this.bytes).buffer;
  }
}
