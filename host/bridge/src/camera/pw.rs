//! A PipeWire video capture stream on the connection the Camera portal
//! hands out. libpipewire is loaded at run time, as `xkbcommon-dl` loads
//! xkbcommon, so the bridge builds without PipeWire's headers and a desktop
//! without PipeWire just has no camera. The structs below mirror the stable
//! C ABI of `pipewire/stream.h` and `spa/buffer/buffer.h`.
//!
//! The stream runs on its own `pw_thread_loop`. Its callbacks run on that
//! thread and call the stream's handler, which must not drop the
//! [`Capture`]: dropping it joins that thread.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::os::fd::{IntoRawFd, OwnedFd};
use std::ptr::null_mut;
use std::sync::OnceLock;

use super::frame::{Plane, Source};
use super::pod::{self, Format, VideoFormat};

const LIB: &CStr = c"libpipewire-0.3.so.0";

#[repr(C)]
struct SpaHook {
    link: [*mut c_void; 2],
    funcs: *const c_void,
    data: *mut c_void,
    removed: *mut c_void,
    private: *mut c_void,
}

type Fn0 = Option<unsafe extern "C" fn(*mut c_void)>;

#[repr(C)]
struct StreamEvents {
    version: u32,
    destroy: Fn0,
    state_changed: Option<unsafe extern "C" fn(*mut c_void, c_int, c_int, *const c_char)>,
    control_info: Fn0,
    io_changed: Fn0,
    param_changed: Option<unsafe extern "C" fn(*mut c_void, u32, *const u8)>,
    add_buffer: Fn0,
    remove_buffer: Fn0,
    process: Fn0,
    drained: Fn0,
    command: Fn0,
    trigger_done: Fn0,
}

#[repr(C)]
struct PwBuffer {
    buffer: *mut SpaBuffer,
}

#[repr(C)]
struct SpaBuffer {
    n_metas: u32,
    n_datas: u32,
    metas: *mut c_void,
    datas: *mut SpaData,
}

#[repr(C)]
struct SpaData {
    ty: u32,
    flags: u32,
    fd: i64,
    mapoffset: u32,
    maxsize: u32,
    data: *mut c_void,
    chunk: *mut SpaChunk,
}

#[repr(C)]
struct SpaChunk {
    offset: u32,
    size: u32,
    stride: i32,
    flags: i32,
}

const STATE_ERROR: c_int = -1;
const STATE_UNCONNECTED: c_int = 0;
const STATE_STREAMING: c_int = 3;
const DIRECTION_INPUT: c_int = 0;
const ID_ANY: u32 = u32::MAX;
const FLAG_AUTOCONNECT: u32 = 1 << 0;
const FLAG_MAP_BUFFERS: u32 = 1 << 2;
const CHUNK_FLAG_CORRUPTED: i32 = 1 << 0;

type P = *mut c_void;

/// A symbol as the function pointer type `T`.
///
/// # Safety
/// `T` must be the symbol's C function pointer type.
unsafe fn cast<T: Copy>(p: *mut c_void) -> T {
    assert_eq!(size_of::<T>(), size_of::<*mut c_void>());
    // SAFETY: Same size, and the caller vouches for the type.
    unsafe { std::mem::transmute_copy::<*mut c_void, T>(&p) }
}

/// The libpipewire functions the stream uses.
struct Lib {
    thread_loop_new: unsafe extern "C" fn(*const c_char, *const c_void) -> P,
    thread_loop_get_loop: unsafe extern "C" fn(P) -> P,
    thread_loop_start: unsafe extern "C" fn(P) -> c_int,
    thread_loop_stop: unsafe extern "C" fn(P),
    thread_loop_destroy: unsafe extern "C" fn(P),
    context_new: unsafe extern "C" fn(P, P, usize) -> P,
    context_destroy: unsafe extern "C" fn(P),
    context_connect_fd: unsafe extern "C" fn(P, c_int, P, usize) -> P,
    core_disconnect: unsafe extern "C" fn(P) -> c_int,
    properties_new_string: unsafe extern "C" fn(*const c_char) -> P,
    stream_new: unsafe extern "C" fn(P, *const c_char, P) -> P,
    stream_add_listener: unsafe extern "C" fn(P, *mut SpaHook, *const StreamEvents, P),
    stream_connect: unsafe extern "C" fn(P, c_int, u32, u32, *mut *const u8, u32) -> c_int,
    stream_dequeue_buffer: unsafe extern "C" fn(P) -> *mut PwBuffer,
    stream_queue_buffer: unsafe extern "C" fn(P, *mut PwBuffer) -> c_int,
    stream_destroy: unsafe extern "C" fn(P),
}

impl Lib {
    fn get() -> Result<&'static Lib, String> {
        static LIB_ONCE: OnceLock<Result<Lib, String>> = OnceLock::new();
        LIB_ONCE
            .get_or_init(Lib::load)
            .as_ref()
            .map_err(Clone::clone)
    }

    fn load() -> Result<Lib, String> {
        // SAFETY: dlopen of a library by soname; the handle is never closed.
        let h = unsafe { libc::dlopen(LIB.as_ptr(), libc::RTLD_NOW | libc::RTLD_LOCAL) };
        if h.is_null() {
            return Err(format!("{} not found", LIB.to_string_lossy()));
        }
        let sym = |name: &CStr| -> Result<*mut c_void, String> {
            // SAFETY: h is a live handle and name is NUL-terminated.
            let p = unsafe { libc::dlsym(h, name.as_ptr()) };
            if p.is_null() {
                Err(format!("libpipewire lacks {}", name.to_string_lossy()))
            } else {
                Ok(p)
            }
        };
        // SAFETY: pw_init takes optional argc and argv.
        unsafe {
            let init: unsafe extern "C" fn(*mut c_int, *mut c_void) = cast(sym(c"pw_init")?);
            init(null_mut(), null_mut());
        }
        // SAFETY: Each symbol has the C signature of the field it fills,
        // from PipeWire's stable API.
        unsafe {
            Ok(Lib {
                thread_loop_new: cast(sym(c"pw_thread_loop_new")?),
                thread_loop_get_loop: cast(sym(c"pw_thread_loop_get_loop")?),
                thread_loop_start: cast(sym(c"pw_thread_loop_start")?),
                thread_loop_stop: cast(sym(c"pw_thread_loop_stop")?),
                thread_loop_destroy: cast(sym(c"pw_thread_loop_destroy")?),
                context_new: cast(sym(c"pw_context_new")?),
                context_destroy: cast(sym(c"pw_context_destroy")?),
                context_connect_fd: cast(sym(c"pw_context_connect_fd")?),
                core_disconnect: cast(sym(c"pw_core_disconnect")?),
                properties_new_string: cast(sym(c"pw_properties_new_string")?),
                stream_new: cast(sym(c"pw_stream_new")?),
                stream_add_listener: cast(sym(c"pw_stream_add_listener")?),
                stream_connect: cast(sym(c"pw_stream_connect")?),
                stream_dequeue_buffer: cast(sym(c"pw_stream_dequeue_buffer")?),
                stream_queue_buffer: cast(sym(c"pw_stream_queue_buffer")?),
                stream_destroy: cast(sym(c"pw_stream_destroy")?),
            })
        }
    }
}

/// What the stream reports to its handler, on the stream's thread.
pub enum Event<'a> {
    /// The stream is linked to a camera and running.
    Streaming,
    Frame(&'a Source<'a>),
    /// The stream failed or the camera went away.
    Ended(String),
}

type Handler = Box<dyn FnMut(Event) + Send>;

struct State {
    hook: SpaHook,
    lib: &'static Lib,
    stream: P,
    format: Option<Format>,
    handler: Handler,
    ended: bool,
}

impl State {
    fn emit(&mut self, e: Event) {
        (self.handler)(e)
    }

    fn end(&mut self, why: String) {
        if !std::mem::replace(&mut self.ended, true) {
            self.emit(Event::Ended(why));
        }
    }
}

static EVENTS: StreamEvents = StreamEvents {
    version: 2,
    destroy: None,
    state_changed: Some(on_state),
    control_info: None,
    io_changed: None,
    param_changed: Some(on_param),
    add_buffer: None,
    remove_buffer: None,
    process: Some(on_process),
    drained: None,
    command: None,
    trigger_done: None,
};

unsafe extern "C" fn on_state(data: P, _old: c_int, new: c_int, error: *const c_char) {
    // SAFETY: data is the State registered with the listener, alive until
    // the stream is destroyed.
    let s = unsafe { &mut *data.cast::<State>() };
    match new {
        STATE_STREAMING => s.emit(Event::Streaming),
        STATE_ERROR => {
            let why = if error.is_null() {
                "stream error".to_owned()
            } else {
                // SAFETY: PipeWire passes a NUL-terminated message.
                unsafe { CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned()
            };
            s.end(why);
        }
        STATE_UNCONNECTED => s.end("the camera went away".into()),
        _ => {}
    }
}

unsafe extern "C" fn on_param(data: P, id: u32, param: *const u8) {
    // SAFETY: As in on_state.
    let s = unsafe { &mut *data.cast::<State>() };
    if id != pod::PARAM_FORMAT {
        return;
    }
    if param.is_null() {
        s.format = None;
        return;
    }
    // SAFETY: A POD starts with its u32 body size and u32 type.
    let size = unsafe { param.cast::<u32>().read_unaligned() } as usize;
    // SAFETY: The POD is its 8-byte header plus `size` bytes.
    let bytes = unsafe { std::slice::from_raw_parts(param, size + 8) };
    s.format = pod::parse_format(bytes);
    match s.format {
        Some(f) => crate::info!("camera: format {f:?}"),
        None => s.end("the camera offered no format the bridge reads".into()),
    }
}

unsafe extern "C" fn on_process(data: P) {
    // SAFETY: As in on_state.
    let s = unsafe { &mut *data.cast::<State>() };
    // SAFETY: The stream is alive while its callbacks run.
    let b = unsafe { (s.lib.stream_dequeue_buffer)(s.stream) };
    if b.is_null() {
        return;
    }
    if let Some(f) = s.format {
        // SAFETY: A dequeued buffer and its datas stay valid until queued.
        let planes = unsafe { planes(&*(*b).buffer, f) };
        if let Some(planes) = planes {
            let src = Source {
                format: f.format,
                width: f.width,
                height: f.height,
                planes,
            };
            s.emit(Event::Frame(&src));
        }
    }
    // SAFETY: b came from this stream.
    unsafe { (s.lib.stream_queue_buffer)(s.stream, b) };
}

/// The planes of a mapped buffer, or None for a corrupt or unmapped one.
///
/// # Safety
/// `b` must be a buffer PipeWire mapped for this stream.
unsafe fn planes(b: &SpaBuffer, f: Format) -> Option<Vec<Plane<'_>>> {
    if b.datas.is_null() || b.n_datas == 0 {
        return None;
    }
    // SAFETY: datas holds n_datas entries.
    let datas = unsafe { std::slice::from_raw_parts(b.datas, b.n_datas.min(4) as usize) };
    let mut out = Vec::with_capacity(datas.len());
    for (i, d) in datas.iter().enumerate() {
        if d.data.is_null() || d.chunk.is_null() {
            return None;
        }
        // SAFETY: chunk points at this data's chunk.
        let c = unsafe { &*d.chunk };
        if c.flags & CHUNK_FLAG_CORRUPTED != 0 {
            return None;
        }
        let off = (c.offset % d.maxsize.max(1)) as usize;
        let len = (c.size as usize).min(d.maxsize as usize - off);
        let stride = match c.stride {
            s if s > 0 => s as usize,
            0 => default_stride(f, i),
            _ => return None,
        };
        // SAFETY: MAP_BUFFERS maps maxsize bytes at data.
        let bytes = unsafe { std::slice::from_raw_parts(d.data.cast::<u8>().add(off), len) };
        out.push(Plane {
            data: bytes,
            stride,
        });
    }
    Some(out)
}

/// The stride of plane `i` when the producer leaves it out.
fn default_stride(f: Format, i: usize) -> usize {
    let w = f.width as usize;
    match (f.format, i) {
        (VideoFormat::Yuy2 | VideoFormat::Uyvy, _) => w.div_ceil(2) * 4,
        (VideoFormat::I420 | VideoFormat::Yv12, 1..) => w.div_ceil(2),
        (VideoFormat::Nv12 | VideoFormat::Nv21, 1..) => w.div_ceil(2) * 2,
        _ => w,
    }
}

/// A running capture stream. Dropping it stops the stream.
pub struct Capture {
    lib: &'static Lib,
    tl: P,
    context: P,
    core: P,
    stream: P,
    _state: Box<State>,
}

// SAFETY: The PipeWire objects are only touched under the thread loop's
// own thread or after it has stopped (Drop).
unsafe impl Send for Capture {}

impl Capture {
    /// Connects to the cameras on `remote` and streams the default one,
    /// preferring `width`x`height`.
    pub fn start(
        remote: OwnedFd,
        width: u32,
        height: u32,
        handler: impl FnMut(Event) + Send + 'static,
    ) -> Result<Capture, String> {
        let lib = Lib::get()?;
        let mut c = Capture {
            lib,
            tl: null_mut(),
            context: null_mut(),
            core: null_mut(),
            stream: null_mut(),
            _state: Box::new(State {
                // SAFETY: An all-zero spa_hook is the unlinked state.
                hook: unsafe { std::mem::zeroed() },
                lib,
                stream: null_mut(),
                format: None,
                handler: Box::new(handler),
                ended: false,
            }),
        };
        let name = c"innkeeper-camera";
        // SAFETY: Each call gets live objects created just before it. Until
        // the thread loop starts, nothing else touches them.
        unsafe {
            c.tl = (lib.thread_loop_new)(name.as_ptr(), std::ptr::null());
            if c.tl.is_null() {
                return Err("pw_thread_loop_new failed".into());
            }
            c.context = (lib.context_new)((lib.thread_loop_get_loop)(c.tl), null_mut(), 0);
            if c.context.is_null() {
                return Err("pw_context_new failed".into());
            }
            c.core = (lib.context_connect_fd)(c.context, remote.into_raw_fd(), null_mut(), 0);
            if c.core.is_null() {
                return Err("the portal's PipeWire connection failed".into());
            }
            let props = CString::new(
                "media.type=Video media.category=Capture media.role=Camera \
                 node.name=innkeeper-camera app.name=Innkeeper",
            )
            .unwrap();
            c.stream = (lib.stream_new)(
                c.core,
                c"Android camera".as_ptr(),
                (lib.properties_new_string)(props.as_ptr()),
            );
            if c.stream.is_null() {
                return Err("pw_stream_new failed".into());
            }
            let state: *mut State = &mut *c._state;
            (*state).stream = c.stream;
            (lib.stream_add_listener)(c.stream, &mut (*state).hook, &EVENTS, state.cast());
            let fmt = pod::enum_format(width, height);
            let mut params = [fmt.as_ptr()];
            let r = (lib.stream_connect)(
                c.stream,
                DIRECTION_INPUT,
                ID_ANY,
                FLAG_AUTOCONNECT | FLAG_MAP_BUFFERS,
                params.as_mut_ptr(),
                1,
            );
            if r < 0 {
                return Err(format!("pw_stream_connect: {}", -r));
            }
            if (lib.thread_loop_start)(c.tl) < 0 {
                return Err("pw_thread_loop_start failed".into());
            }
        }
        Ok(c)
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        let l = self.lib;
        // SAFETY: Stopping the loop joins its thread, so nothing else uses
        // these objects afterwards. Each is destroyed once, children first.
        unsafe {
            if !self.tl.is_null() {
                (l.thread_loop_stop)(self.tl);
            }
            if !self.stream.is_null() {
                (l.stream_destroy)(self.stream);
            }
            if !self.core.is_null() {
                (l.core_disconnect)(self.core);
            }
            if !self.context.is_null() {
                (l.context_destroy)(self.context);
            }
            if !self.tl.is_null() {
                (l.thread_loop_destroy)(self.tl);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_strides() {
        let f = |format, width| Format {
            format,
            width,
            height: 2,
        };
        assert_eq!(default_stride(f(VideoFormat::Yuy2, 641), 0), 1284);
        assert_eq!(default_stride(f(VideoFormat::Nv12, 641), 0), 641);
        assert_eq!(default_stride(f(VideoFormat::Nv12, 641), 1), 642);
        assert_eq!(default_stride(f(VideoFormat::I420, 641), 2), 321);
    }

    #[test]
    fn buffer_planes() {
        let mut bytes = [7u8; 64];
        let mut chunk = SpaChunk {
            offset: 8,
            size: 32,
            stride: 0,
            flags: 0,
        };
        let mut d = SpaData {
            ty: 1,
            flags: 0,
            fd: -1,
            mapoffset: 0,
            maxsize: 64,
            data: bytes.as_mut_ptr().cast(),
            chunk: &mut chunk,
        };
        let b = SpaBuffer {
            n_metas: 0,
            n_datas: 1,
            metas: null_mut(),
            datas: &mut d,
        };
        let f = Format {
            format: VideoFormat::Yuy2,
            width: 4,
            height: 2,
        };
        let p = unsafe { planes(&b, f) }.unwrap();
        assert_eq!((p[0].data.len(), p[0].stride), (32, 8));
        // A size past the mapping is cut to it.
        unsafe { (*d.chunk).size = 1000 };
        let p = unsafe { planes(&b, f) }.unwrap();
        assert_eq!(p[0].data.len(), 56);
        unsafe { (*d.chunk).flags = CHUNK_FLAG_CORRUPTED };
        assert!(unsafe { planes(&b, f) }.is_none());
        unsafe { (*d.chunk).flags = 0 };
        unsafe { (*d.chunk).stride = -8 };
        assert!(unsafe { planes(&b, f) }.is_none());
    }
}
