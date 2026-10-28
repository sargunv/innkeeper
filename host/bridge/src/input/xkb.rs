//! The host's keyboard layout, compose and dead keys: what each key press
//! means under the compositor's xkb keymap. Follows winit's
//! `platform_impl/linux/common/xkb`: libxkbcommon is loaded at run time
//! (`xkbcommon-dl`), the state follows the compositor's `modifiers` events
//! only, and one compose state from the locale's table is fed each press.
//!
//! Every press resolves to the key's level-1 keysym in the active layout
//! (the system app picks the Android keycode from it, so shortcuts follow
//! the layout) and to what it types: text, nothing, or a step of a compose
//! sequence (a dead key, `Multi_key`). `docs/input.md` has the whole path.

use std::ffi::{CStr, CString, c_char};
use std::os::fd::OwnedFd;
use std::ptr::NonNull;

use xkbcommon_dl::{
    XkbCommon, XkbCommonCompose, xkb_compose_compile_flags, xkb_compose_feed_result,
    xkb_compose_state, xkb_compose_state_flags, xkb_compose_status, xkb_compose_table, xkb_context,
    xkb_context_flags, xkb_keymap, xkb_keymap_compile_flags, xkb_keymap_format, xkb_state,
};

/// evdev codes are xkb keycodes minus 8.
const EVDEV_OFFSET: u32 = 8;
/// Longest text one press may type (a compose sequence's result).
pub const MAX_TEXT: usize = 64;

/// What a key press types.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    /// Nothing (not a character key, or a Ctrl chord's control character).
    Nothing,
    /// This text.
    Text(String),
    /// A step of a compose sequence (a dead key, `Multi_key`, or a key that
    /// cancelled one): it types nothing itself.
    Compose,
}

/// A key press under the host layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Press {
    /// The key's level-1 keysym (no modifiers) in the active layout; 0 if
    /// it has none.
    pub sym: u32,
    pub typed: Typed,
}

pub struct Xkb {
    xkb: &'static XkbCommon,
    ctx: NonNull<xkb_context>,
    keymap: Option<Keymap>,
    compose: Option<Compose>,
}

struct Keymap {
    xkb: &'static XkbCommon,
    keymap: NonNull<xkb_keymap>,
    state: NonNull<xkb_state>,
}

struct Compose {
    xkbc: &'static XkbCommonCompose,
    table: NonNull<xkb_compose_table>,
    state: NonNull<xkb_compose_state>,
}

// The pointers are owned and only used from the thread that owns the value.
unsafe impl Send for Xkb {}

impl Xkb {
    /// None without libxkbcommon; then keys go out as evdev codes only.
    pub fn new() -> Option<Xkb> {
        let xkb = xkbcommon_dl::xkbcommon_option()?;
        // Keymaps come from the compositor (or, in tests, explicit names),
        // never from XKB_DEFAULT_* in our environment.
        let ctx = NonNull::new(unsafe {
            (xkb.xkb_context_new)(xkb_context_flags::XKB_CONTEXT_NO_ENVIRONMENT_NAMES)
        })?;
        let compose = Compose::from_locale(ctx);
        if compose.is_none() {
            crate::info!("input: no compose table for the locale; compose and dead keys are off");
        }
        Some(Xkb {
            xkb,
            ctx,
            keymap: None,
            compose,
        })
    }

    /// `wl_keyboard.keymap` (format xkb_v1): a private mapping of `size`
    /// bytes, NUL-terminated.
    pub fn set_keymap_fd(&mut self, fd: OwnedFd, size: u32) -> bool {
        self.keymap = None;
        if size == 0 {
            return false;
        }
        let size = size as usize;
        // MAP_PRIVATE: compositors since wl_keyboard v7 hand out read-only fds.
        let map = match unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                size,
                rustix::mm::ProtFlags::READ,
                rustix::mm::MapFlags::PRIVATE,
                &fd,
                0,
            )
        } {
            Ok(p) => p,
            Err(e) => {
                crate::info!("input: can't map the keymap: {e}");
                return false;
            }
        };
        let bytes = unsafe { std::slice::from_raw_parts(map as *const u8, size) };
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(size);
        let km = unsafe {
            (self.xkb.xkb_keymap_new_from_buffer)(
                self.ctx.as_ptr(),
                map as *const c_char,
                len,
                xkb_keymap_format::XKB_KEYMAP_FORMAT_TEXT_V1,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        let _ = unsafe { rustix::mm::munmap(map, size) };
        self.install(km)
    }

    /// A keymap from RMLVO names (tests, and hosts without a compositor
    /// keymap).
    pub fn set_keymap_names(&mut self, layout: &str, variant: &str, options: &str) -> bool {
        let c = |s: &str| CString::new(s).unwrap_or_default();
        let (layout, variant, options) = (c(layout), c(variant), c(options));
        let names = xkbcommon_dl::xkb_rule_names {
            rules: std::ptr::null(),
            model: std::ptr::null(),
            layout: layout.as_ptr(),
            variant: variant.as_ptr(),
            options: options.as_ptr(),
        };
        let km = unsafe {
            (self.xkb.xkb_keymap_new_from_names)(
                self.ctx.as_ptr(),
                &names,
                xkb_keymap_compile_flags::XKB_KEYMAP_COMPILE_NO_FLAGS,
            )
        };
        self.install(km)
    }

    fn install(&mut self, km: *mut xkb_keymap) -> bool {
        self.keymap = Keymap::new(self.xkb, km);
        self.reset_compose();
        self.keymap.is_some()
    }

    pub fn has_keymap(&self) -> bool {
        self.keymap.is_some()
    }

    /// `wl_keyboard.modifiers`: the compositor's state is the only source.
    pub fn update_mask(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        if let Some(k) = &self.keymap {
            unsafe {
                (self.xkb.xkb_state_update_mask)(
                    k.state.as_ptr(),
                    depressed,
                    latched,
                    locked,
                    0,
                    0,
                    group,
                )
            };
        }
    }

    /// Focus changed: a half-typed compose sequence is dropped.
    pub fn reset_compose(&mut self) {
        if let Some(c) = &self.compose {
            unsafe { (c.xkbc.xkb_compose_state_reset)(c.state.as_ptr()) };
        }
    }

    /// A key repeat: its base keysym and what it types, without compose.
    pub fn repeat(&self, evdev: u32) -> Option<Press> {
        let k = self.keymap.as_ref()?;
        let kc = evdev + EVDEV_OFFSET;
        Some(Press {
            sym: k.base_sym(kc),
            typed: text(k.utf8(kc)),
        })
    }

    /// A key press: its base keysym and what it types. Feeds compose.
    pub fn press(&mut self, evdev: u32) -> Option<Press> {
        let k = self.keymap.as_ref()?;
        let kc = evdev + EVDEV_OFFSET;
        let sym = k.base_sym(kc);
        let one = unsafe { (self.xkb.xkb_state_key_get_one_sym)(k.state.as_ptr(), kc) };
        let typed = match &self.compose {
            Some(c) => match c.feed(one) {
                None => text(k.utf8(kc)),
                Some(xkb_compose_status::XKB_COMPOSE_COMPOSING) => Typed::Compose,
                Some(xkb_compose_status::XKB_COMPOSE_CANCELLED) => {
                    c.reset();
                    Typed::Compose
                }
                Some(xkb_compose_status::XKB_COMPOSE_COMPOSED) => {
                    let s = c.utf8();
                    c.reset();
                    match text(s) {
                        // A sequence whose result is a keysym without text.
                        Typed::Nothing => Typed::Compose,
                        t => t,
                    }
                }
                Some(xkb_compose_status::XKB_COMPOSE_NOTHING) => text(k.utf8(kc)),
            },
            None => text(k.utf8(kc)),
        };
        Some(Press { sym, typed })
    }
}

/// Text without control characters (Return, Tab, Ctrl+letter chords), and
/// bounded.
fn text(s: String) -> Typed {
    if s.is_empty() || s.len() > MAX_TEXT || s.chars().any(char::is_control) {
        Typed::Nothing
    } else {
        Typed::Text(s)
    }
}

impl Drop for Xkb {
    fn drop(&mut self) {
        self.keymap = None;
        self.compose = None;
        unsafe { (self.xkb.xkb_context_unref)(self.ctx.as_ptr()) };
    }
}

impl Keymap {
    fn new(xkb: &'static XkbCommon, km: *mut xkb_keymap) -> Option<Keymap> {
        let keymap = NonNull::new(km)?;
        match NonNull::new(unsafe { (xkb.xkb_state_new)(keymap.as_ptr()) }) {
            Some(state) => Some(Keymap { xkb, keymap, state }),
            None => {
                unsafe { (xkb.xkb_keymap_unref)(keymap.as_ptr()) };
                None
            }
        }
    }

    fn base_sym(&self, kc: u32) -> u32 {
        let layout = unsafe { (self.xkb.xkb_state_key_get_layout)(self.state.as_ptr(), kc) };
        if layout == xkbcommon_dl::XKB_LAYOUT_INVALID {
            return 0;
        }
        let mut syms: *const u32 = std::ptr::null();
        let n = unsafe {
            (self.xkb.xkb_keymap_key_get_syms_by_level)(
                self.keymap.as_ptr(),
                kc,
                layout,
                0,
                &mut syms,
            )
        };
        if n > 0 && !syms.is_null() {
            unsafe { *syms }
        } else {
            0
        }
    }

    fn utf8(&self, kc: u32) -> String {
        read_utf8(|buf, len| unsafe {
            (self.xkb.xkb_state_key_get_utf8)(self.state.as_ptr(), kc, buf, len)
        })
    }
}

impl Drop for Keymap {
    fn drop(&mut self) {
        unsafe {
            (self.xkb.xkb_state_unref)(self.state.as_ptr());
            (self.xkb.xkb_keymap_unref)(self.keymap.as_ptr());
        }
    }
}

impl Compose {
    /// The table for the locale (LC_ALL, LC_CTYPE, LANG, as libc picks it),
    /// or en_US.UTF-8's when the locale has none (C, unset).
    fn from_locale(ctx: NonNull<xkb_context>) -> Option<Compose> {
        let xkbc = xkbcommon_dl::xkbcommon_compose_option()?;
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .iter()
            .filter_map(|v| std::env::var(v).ok())
            .find(|v| !v.is_empty())
            .unwrap_or_default();
        let new = |l: &str| {
            let l = CString::new(l).ok()?;
            NonNull::new(unsafe {
                (xkbc.xkb_compose_table_new_from_locale)(
                    ctx.as_ptr(),
                    l.as_ptr(),
                    xkb_compose_compile_flags::XKB_COMPOSE_COMPILE_NO_FLAGS,
                )
            })
        };
        let table = new(&locale).or_else(|| new("en_US.UTF-8"))?;
        let state = unsafe {
            (xkbc.xkb_compose_state_new)(
                table.as_ptr(),
                xkb_compose_state_flags::XKB_COMPOSE_STATE_NO_FLAGS,
            )
        };
        match NonNull::new(state) {
            Some(state) => Some(Compose { xkbc, table, state }),
            None => {
                unsafe { (xkbc.xkb_compose_table_unref)(table.as_ptr()) };
                None
            }
        }
    }

    /// None if compose ignored the keysym (modifiers), else the new status.
    fn feed(&self, sym: u32) -> Option<xkb_compose_status> {
        match unsafe { (self.xkbc.xkb_compose_state_feed)(self.state.as_ptr(), sym) } {
            xkb_compose_feed_result::XKB_COMPOSE_FEED_IGNORED => None,
            xkb_compose_feed_result::XKB_COMPOSE_FEED_ACCEPTED => {
                Some(unsafe { (self.xkbc.xkb_compose_state_get_status)(self.state.as_ptr()) })
            }
        }
    }

    fn reset(&self) {
        unsafe { (self.xkbc.xkb_compose_state_reset)(self.state.as_ptr()) };
    }

    fn utf8(&self) -> String {
        read_utf8(|buf, len| unsafe {
            (self.xkbc.xkb_compose_state_get_utf8)(self.state.as_ptr(), buf, len)
        })
    }
}

impl Drop for Compose {
    fn drop(&mut self) {
        unsafe {
            (self.xkbc.xkb_compose_state_unref)(self.state.as_ptr());
            (self.xkbc.xkb_compose_table_unref)(self.table.as_ptr());
        }
    }
}

/// libxkbcommon's snprintf-style string getters: the length without the
/// NUL, then the string.
fn read_utf8(f: impl Fn(*mut c_char, usize) -> i32) -> String {
    let n = f(std::ptr::null_mut(), 0);
    if n <= 0 {
        return String::new();
    }
    let mut buf = vec![0u8; n as usize + 1];
    if f(buf.as_mut_ptr() as *mut c_char, buf.len()) != n {
        return String::new();
    }
    CStr::from_bytes_until_nul(&buf)
        .ok()
        .and_then(|c| c.to_str().ok())
        .map(str::to_owned)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    //! Keymaps compiled from RMLVO names, and a model of the compositor: its
    //! own xkb state takes the key presses and serializes the masks that
    //! `wl_keyboard.modifiers` would carry.
    use super::*;
    use xkbcommon_dl::{xkb_key_direction, xkb_state_component};

    const KEY_1: u32 = 2;
    const KEY_2: u32 = 3;
    const KEY_Q: u32 = 16;
    const KEY_W: u32 = 17;
    const KEY_E: u32 = 18;
    const KEY_Y: u32 = 21;
    const KEY_LEFTBRACE: u32 = 26;
    const KEY_GRAVE: u32 = 41;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_Z: u32 = 44;
    const KEY_N: u32 = 49;
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_RIGHTALT: u32 = 100;
    const KEY_ENTER: u32 = 28;

    /// The client side (ours) and a compositor-side state on the same names.
    struct Rig {
        ours: Xkb,
        comp: Xkb,
    }

    impl Rig {
        fn new(layout: &str, options: &str) -> Option<Rig> {
            let (mut ours, mut comp) = (Xkb::new()?, Xkb::new()?);
            assert!(
                ours.set_keymap_names(layout, "", options),
                "keymap {layout}"
            );
            assert!(comp.set_keymap_names(layout, "", options));
            Some(Rig { ours, comp })
        }

        /// The compositor sees the key; we get `modifiers` after it, as on
        /// Wayland (the key event itself is resolved with the old state).
        fn key(&mut self, evdev: u32, down: bool) -> Option<Press> {
            let p = if down { self.ours.press(evdev) } else { None };
            let k = self.comp.keymap.as_ref().unwrap();
            let x = self.comp.xkb;
            let dir = if down {
                xkb_key_direction::XKB_KEY_DOWN
            } else {
                xkb_key_direction::XKB_KEY_UP
            };
            let (dep, lat, lock, group) = unsafe {
                (x.xkb_state_update_key)(k.state.as_ptr(), evdev + EVDEV_OFFSET, dir);
                let s = k.state.as_ptr();
                (
                    (x.xkb_state_serialize_mods)(s, xkb_state_component::XKB_STATE_MODS_DEPRESSED),
                    (x.xkb_state_serialize_mods)(s, xkb_state_component::XKB_STATE_MODS_LATCHED),
                    (x.xkb_state_serialize_mods)(s, xkb_state_component::XKB_STATE_MODS_LOCKED),
                    (x.xkb_state_serialize_layout)(
                        s,
                        xkb_state_component::XKB_STATE_LAYOUT_EFFECTIVE,
                    ),
                )
            };
            self.ours.update_mask(dep, lat, lock, group);
            p
        }

        fn tap(&mut self, evdev: u32) -> Press {
            let p = self.key(evdev, true).unwrap();
            self.key(evdev, false);
            p
        }
    }

    fn rig(layout: &str, options: &str) -> Option<Rig> {
        let r = Rig::new(layout, options);
        if r.is_none() {
            eprintln!("libxkbcommon not available; skipping");
        }
        r
    }

    fn typed(s: &str) -> Typed {
        Typed::Text(s.into())
    }

    #[test]
    fn letters_follow_the_layout() {
        let Some(mut fr) = rig("fr", "") else { return };
        // AZERTY: the Q position is 'a', the W position 'z'.
        assert_eq!(
            fr.tap(KEY_Q),
            Press {
                sym: 'a' as u32,
                typed: typed("a")
            }
        );
        assert_eq!(fr.tap(KEY_W).sym, 'z' as u32);
        // The digit row types symbols unshifted.
        assert_eq!(
            fr.tap(KEY_2),
            Press {
                sym: 0xe9,
                typed: typed("é")
            }
        );
        fr.key(KEY_LEFTSHIFT, true);
        assert_eq!(
            fr.tap(KEY_1),
            Press {
                sym: '&' as u32,
                typed: typed("1")
            }
        );
        fr.key(KEY_LEFTSHIFT, false);

        let Some(mut de) = rig("de", "") else { return };
        // QWERTZ: Y and Z swap.
        assert_eq!(
            de.tap(KEY_Y),
            Press {
                sym: 'z' as u32,
                typed: typed("z")
            }
        );
        assert_eq!(de.tap(KEY_Z).sym, 'y' as u32);
    }

    #[test]
    fn shift_changes_the_text_not_the_base_keysym() {
        let Some(mut us) = rig("us", "") else { return };
        us.key(KEY_LEFTSHIFT, true);
        assert_eq!(
            us.tap(KEY_Q),
            Press {
                sym: 'q' as u32,
                typed: typed("Q")
            }
        );
    }

    #[test]
    fn chords_and_control_keys_type_nothing() {
        let Some(mut fr) = rig("fr", "") else { return };
        fr.key(KEY_LEFTCTRL, true);
        // Ctrl+Z on AZERTY: the 'z' keysym, and a control character.
        assert_eq!(
            fr.tap(KEY_W),
            Press {
                sym: 'z' as u32,
                typed: Typed::Nothing
            }
        );
        fr.key(KEY_LEFTCTRL, false);
        assert_eq!(fr.tap(KEY_ENTER).typed, Typed::Nothing);
        assert_eq!(fr.tap(KEY_LEFTSHIFT).typed, Typed::Nothing);
    }

    #[test]
    fn dead_keys_compose() {
        let Some(mut fr) = rig("fr", "") else { return };
        // The circumflex is dead on AZERTY's [ key.
        assert_eq!(fr.tap(KEY_LEFTBRACE).typed, Typed::Compose);
        assert_eq!(
            fr.tap(KEY_E),
            Press {
                sym: 'e' as u32,
                typed: typed("ê")
            }
        );
        assert_eq!(fr.tap(KEY_E).typed, typed("e"));
        // A key that can't follow cancels the sequence and types nothing.
        assert_eq!(fr.tap(KEY_LEFTBRACE).typed, Typed::Compose);
        assert_eq!(fr.tap(KEY_ENTER).typed, Typed::Compose);
        assert_eq!(fr.tap(KEY_E).typed, typed("e"));
    }

    #[test]
    fn the_compose_key_composes() {
        let Some(mut us) = rig("us", "compose:ralt") else {
            return;
        };
        assert_eq!(us.tap(KEY_RIGHTALT).typed, Typed::Compose);
        us.key(KEY_LEFTSHIFT, true);
        assert_eq!(us.tap(KEY_GRAVE).typed, Typed::Compose); // ~
        us.key(KEY_LEFTSHIFT, false);
        assert_eq!(
            us.tap(KEY_N),
            Press {
                sym: 'n' as u32,
                typed: typed("ñ")
            }
        );
    }

    #[test]
    fn focus_changes_drop_a_half_typed_sequence() {
        let Some(mut fr) = rig("fr", "") else { return };
        fr.tap(KEY_LEFTBRACE);
        fr.ours.reset_compose();
        assert_eq!(fr.tap(KEY_E).typed, typed("e"));
    }

    #[test]
    fn the_active_group_picks_the_layout() {
        let Some(mut r) = rig("us,de", "") else {
            return;
        };
        assert_eq!(r.tap(KEY_Y).sym, 'y' as u32);
        // The compositor switched to the second layout.
        r.ours.update_mask(0, 0, 0, 1);
        assert_eq!(r.tap(KEY_Y).sym, 'z' as u32);
    }
}
