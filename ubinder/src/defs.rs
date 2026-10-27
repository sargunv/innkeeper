//! Binder UAPI constants and wire structs.
//!
//! Mirror of `include/uapi/linux/android/binder.h` for the x86_64 (64-bit
//! `binder_uintptr_t`) ABI that the AOSP 17 image uses: the pieces the broker
//! uses, and the freezer's. Structs are `#[repr(C)]`, so their bytes are what
//! libbinder writes into the command stream and reads back.

pub type BinderSize = u64;
pub type BinderUintptr = u64;

pub const BINDER_CURRENT_PROTOCOL_VERSION: i32 = 8;

// `_IOC` encoding (asm-generic): dir(2) size(14) type(8) nr(8).
const fn ioc(dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
    const NRBITS: u32 = 8;
    const TYPEBITS: u32 = 8;
    const SIZEBITS: u32 = 14;
    const NRSHIFT: u32 = 0;
    const TYPESHIFT: u32 = NRSHIFT + NRBITS;
    const SIZESHIFT: u32 = TYPESHIFT + TYPEBITS;
    const DIRSHIFT: u32 = SIZESHIFT + SIZEBITS;
    (dir << DIRSHIFT)
        | ((ty as u32) << TYPESHIFT)
        | ((nr as u32) << NRSHIFT)
        | ((size as u32) << SIZESHIFT)
}
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const fn iow(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(IOC_WRITE, ty, nr, size)
}
const fn ior(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(IOC_READ, ty, nr, size)
}
const fn iowr(ty: u8, nr: u8, size: usize) -> u32 {
    ioc(IOC_WRITE | IOC_READ, ty, nr, size)
}

// BINDER_* ioctls (type 'b' = 0x62).
pub const BINDER_WRITE_READ: u32 = iowr(b'b', 1, size_of::<BinderWriteRead>());
pub const BINDER_SET_MAX_THREADS: u32 = iow(b'b', 5, size_of::<u32>());
pub const BINDER_SET_CONTEXT_MGR: u32 = iow(b'b', 7, size_of::<i32>());
pub const BINDER_THREAD_EXIT: u32 = iow(b'b', 8, size_of::<i32>());
pub const BINDER_VERSION: u32 = iowr(b'b', 9, size_of::<BinderVersion>());
pub const BINDER_GET_NODE_DEBUG_INFO: u32 = iowr(b'b', 11, size_of::<BinderNodeDebugInfo>());
pub const BINDER_GET_NODE_INFO_FOR_REF: u32 = iowr(b'b', 12, size_of::<BinderNodeInfoForRef>());
pub const BINDER_SET_CONTEXT_MGR_EXT: u32 = iow(b'b', 13, size_of::<FlatBinderObject>());
pub const BINDER_ENABLE_ONEWAY_SPAM_DETECTION: u32 = iow(b'b', 16, size_of::<u32>());
pub const BINDER_GET_EXTENDED_ERROR: u32 = iowr(b'b', 17, size_of::<BinderExtendedError>());

/// Is `cmd` one of the binder ioctls? (used by the seccomp dispatcher)
pub fn is_binder_ioctl(cmd: u32) -> bool {
    // All share type 'b'; match on type byte to be tolerant of size drift.
    ((cmd >> 8) & 0xff) == b'b' as u32
}

// Driver command protocol (userspace -> driver), type 'c'.
pub const BC_TRANSACTION: u32 = iow(b'c', 0, size_of::<BinderTransactionData>());
pub const BC_REPLY: u32 = iow(b'c', 1, size_of::<BinderTransactionData>());
pub const BC_ACQUIRE_RESULT: u32 = iow(b'c', 2, size_of::<i32>());
pub const BC_FREE_BUFFER: u32 = iow(b'c', 3, size_of::<BinderUintptr>());
pub const BC_INCREFS: u32 = iow(b'c', 4, size_of::<u32>());
pub const BC_ACQUIRE: u32 = iow(b'c', 5, size_of::<u32>());
pub const BC_RELEASE: u32 = iow(b'c', 6, size_of::<u32>());
pub const BC_DECREFS: u32 = iow(b'c', 7, size_of::<u32>());
pub const BC_INCREFS_DONE: u32 = iow(b'c', 8, size_of::<BinderPtrCookie>());
pub const BC_ACQUIRE_DONE: u32 = iow(b'c', 9, size_of::<BinderPtrCookie>());
pub const BC_ATTEMPT_ACQUIRE: u32 = iow(b'c', 10, size_of::<BinderPriDesc>());
pub const BC_REGISTER_LOOPER: u32 = ioc(IOC_NONE, b'c', 11, 0);
pub const BC_ENTER_LOOPER: u32 = ioc(IOC_NONE, b'c', 12, 0);
pub const BC_EXIT_LOOPER: u32 = ioc(IOC_NONE, b'c', 13, 0);
pub const BC_REQUEST_DEATH_NOTIFICATION: u32 = iow(b'c', 14, size_of::<BinderHandleCookie>());
pub const BC_CLEAR_DEATH_NOTIFICATION: u32 = iow(b'c', 15, size_of::<BinderHandleCookie>());
pub const BC_DEAD_BINDER_DONE: u32 = iow(b'c', 16, size_of::<BinderUintptr>());
pub const BC_TRANSACTION_SG: u32 = iow(b'c', 17, size_of::<BinderTransactionDataSg>());
pub const BC_REPLY_SG: u32 = iow(b'c', 18, size_of::<BinderTransactionDataSg>());
pub const BC_REQUEST_FREEZE_NOTIFICATION: u32 = iow(b'c', 19, size_of::<BinderHandleCookie>());
pub const BC_CLEAR_FREEZE_NOTIFICATION: u32 = iow(b'c', 20, size_of::<BinderHandleCookie>());
pub const BC_FREEZE_NOTIFICATION_DONE: u32 = iow(b'c', 21, size_of::<BinderUintptr>());

// Driver return protocol (driver -> userspace), type 'r'.
pub const BR_ERROR: u32 = ior(b'r', 0, size_of::<i32>());
pub const BR_OK: u32 = ioc(IOC_NONE, b'r', 1, 0);
pub const BR_TRANSACTION_SEC_CTX: u32 = ior(b'r', 2, size_of::<BinderTransactionDataSecctx>());
pub const BR_TRANSACTION: u32 = ior(b'r', 2, size_of::<BinderTransactionData>());
pub const BR_REPLY: u32 = ior(b'r', 3, size_of::<BinderTransactionData>());
pub const BR_DEAD_REPLY: u32 = ioc(IOC_NONE, b'r', 5, 0);
pub const BR_TRANSACTION_COMPLETE: u32 = ioc(IOC_NONE, b'r', 6, 0);
pub const BR_INCREFS: u32 = ior(b'r', 7, size_of::<BinderPtrCookie>());
pub const BR_ACQUIRE: u32 = ior(b'r', 8, size_of::<BinderPtrCookie>());
pub const BR_RELEASE: u32 = ior(b'r', 9, size_of::<BinderPtrCookie>());
pub const BR_DECREFS: u32 = ior(b'r', 10, size_of::<BinderPtrCookie>());
pub const BR_NOOP: u32 = ioc(IOC_NONE, b'r', 12, 0);
pub const BR_SPAWN_LOOPER: u32 = ioc(IOC_NONE, b'r', 13, 0);
pub const BR_DEAD_BINDER: u32 = ior(b'r', 15, size_of::<BinderUintptr>());
pub const BR_CLEAR_DEATH_NOTIFICATION_DONE: u32 = ior(b'r', 16, size_of::<BinderUintptr>());
pub const BR_FAILED_REPLY: u32 = ioc(IOC_NONE, b'r', 17, 0);
pub const BR_ONEWAY_SPAM_SUSPECT: u32 = ioc(IOC_NONE, b'r', 19, 0);

// transaction_flags
pub const TF_ONE_WAY: u32 = 0x01;
pub const TF_ACCEPT_FDS: u32 = 0x10;

// flat_binder_object flags
pub const FLAT_BINDER_FLAG_ACCEPTS_FDS: u32 = 0x100;
pub const FLAT_BINDER_FLAG_TXN_SECURITY_CTX: u32 = 0x1000;

pub const BINDER_BUFFER_FLAG_HAS_PARENT: u32 = 0x01;

// Object header types (B_PACK_CHARS).
const fn pack(c1: u8, c2: u8, c3: u8, c4: u8) -> u32 {
    ((c1 as u32) << 24) | ((c2 as u32) << 16) | ((c3 as u32) << 8) | (c4 as u32)
}
const B_TYPE_LARGE: u8 = 0x85;
pub const BINDER_TYPE_BINDER: u32 = pack(b's', b'b', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_WEAK_BINDER: u32 = pack(b'w', b'b', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_HANDLE: u32 = pack(b's', b'h', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_WEAK_HANDLE: u32 = pack(b'w', b'h', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_FD: u32 = pack(b'f', b'd', b'*', B_TYPE_LARGE);
pub const BINDER_TYPE_FDA: u32 = pack(b'f', b'd', b'a', B_TYPE_LARGE);
pub const BINDER_TYPE_PTR: u32 = pack(b'p', b't', b'*', B_TYPE_LARGE);

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderWriteRead {
    pub write_size: BinderSize,
    pub write_consumed: BinderSize,
    pub write_buffer: BinderUintptr,
    pub read_size: BinderSize,
    pub read_consumed: BinderSize,
    pub read_buffer: BinderUintptr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderVersion {
    pub protocol_version: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct FlatBinderObject {
    pub hdr_type: u32,
    pub flags: u32,
    /// union: binder (local ptr) or handle
    pub binder_or_handle: u64,
    pub cookie: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderFdObject {
    pub hdr_type: u32,
    pub pad_flags: u32,
    /// union: pad_binder / fd (low 32 bits)
    pub fd_or_pad: u64,
    pub cookie: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderBufferObject {
    pub hdr_type: u32,
    pub flags: u32,
    pub buffer: BinderUintptr,
    pub length: BinderSize,
    pub parent: BinderSize,
    pub parent_offset: BinderSize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderFdArrayObject {
    pub hdr_type: u32,
    pub pad: u32,
    pub num_fds: BinderSize,
    pub parent: BinderSize,
    pub parent_offset: BinderSize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderTransactionData {
    /// union target: handle (u32) or ptr (u64)
    pub target: u64,
    pub cookie: u64,
    pub code: u32,
    pub flags: u32,
    pub sender_pid: i32,
    pub sender_euid: u32,
    pub data_size: BinderSize,
    pub offsets_size: BinderSize,
    /// data.ptr.buffer
    pub data_buffer: BinderUintptr,
    /// data.ptr.offsets
    pub data_offsets: BinderUintptr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderTransactionDataSecctx {
    pub transaction_data: BinderTransactionData,
    pub secctx: BinderUintptr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderTransactionDataSg {
    pub transaction_data: BinderTransactionData,
    pub buffers_size: BinderSize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderPtrCookie {
    pub ptr: BinderUintptr,
    pub cookie: BinderUintptr,
}

/// `__packed` in the UAPI (12 bytes): the size is part of the
/// BC_*_DEATH_NOTIFICATION command codes.
#[repr(C, packed)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderHandleCookie {
    pub handle: u32,
    pub cookie: BinderUintptr,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderPriDesc {
    pub priority: i32,
    pub desc: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderNodeDebugInfo {
    pub ptr: BinderUintptr,
    pub cookie: BinderUintptr,
    pub has_strong_ref: u32,
    pub has_weak_ref: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderNodeInfoForRef {
    pub handle: u32,
    pub strong_count: u32,
    pub weak_count: u32,
    pub reserved1: u32,
    pub reserved2: u32,
    pub reserved3: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderExtendedError {
    pub id: u32,
    pub command: u32,
    pub param: i32,
}

// The freezer (BINDER_FREEZE) is unsupported: the ioctls fail with EINVAL, so
// Android disables its app freezer, and the broker acknowledges freeze
// notification requests without ever sending one. These are the pieces an
// implementation needs (binder freezer issue).
pub const BINDER_FREEZE: u32 = iow(b'b', 14, size_of::<BinderFreezeInfo>());
pub const BINDER_GET_FROZEN_INFO: u32 = iowr(b'b', 15, size_of::<BinderFrozenStatusInfo>());
pub const BR_FROZEN_REPLY: u32 = ioc(IOC_NONE, b'r', 18, 0);
pub const BR_TRANSACTION_PENDING_FROZEN: u32 = ioc(IOC_NONE, b'r', 20, 0);
pub const BR_FROZEN_BINDER: u32 = ior(b'r', 21, size_of::<BinderFrozenStateInfo>());
pub const BR_CLEAR_FREEZE_NOTIFICATION_DONE: u32 = ior(b'r', 22, size_of::<BinderUintptr>());

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderFreezeInfo {
    pub pid: u32,
    pub enable: u32,
    pub timeout_ms: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderFrozenStatusInfo {
    pub pid: u32,
    pub sync_recv: u32,
    pub async_recv: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct BinderFrozenStateInfo {
    pub cookie: BinderUintptr,
    pub is_frozen: u32,
    pub reserved: u32,
}

/// Read/write a little-endian POD from/to a byte slice.
pub(crate) fn read_pod<T: Copy>(buf: &[u8], off: usize) -> Option<T> {
    let sz = size_of::<T>();
    if off.checked_add(sz)? > buf.len() {
        return None;
    }
    // SAFETY: bounds checked; T is a #[repr(C)] POD and x86_64 is LE so the
    // wire bytes match the in-memory layout.
    let mut v = std::mem::MaybeUninit::<T>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(buf.as_ptr().add(off), v.as_mut_ptr() as *mut u8, sz);
        Some(v.assume_init())
    }
}

pub(crate) fn write_pod<T: Copy>(buf: &mut Vec<u8>, v: &T) {
    let sz = size_of::<T>();
    let p = v as *const T as *const u8;
    // SAFETY: reading sz bytes of a Copy POD.
    buf.extend_from_slice(unsafe { std::slice::from_raw_parts(p, sz) });
}

pub(crate) fn pod_bytes<T: Copy>(v: &T) -> &[u8] {
    let p = v as *const T as *const u8;
    unsafe { std::slice::from_raw_parts(p, size_of::<T>()) }
}

pub(crate) fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}
