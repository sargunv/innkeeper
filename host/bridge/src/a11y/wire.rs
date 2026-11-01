//! The snapshot blob of a TREE request. It holds one window's
//! AccessibilityNodeInfo tree, or a part of it, as the system app's
//! `A11ySnapshot.kt` writes it. The fields are those of Chromium's ARC++
//! `accessibility_helper.mojom` (`AccessibilityNodeInfoData`), which
//! [`super::tree`] maps to AccessKit as Chromium does. The encoding is
//! little-endian, and `str` is `u16 len` + UTF-8 without NUL.
//!
//! | Part   | Layout |
//! | ------ | ------ |
//! | header | `u16 version` (1), `u16 flags` (bit 0: partial), `f32 scale` (display pixels per logical pixel, 0.25..=16), `u64 focus` (the node with input focus; 0: none, or in a partial snapshot unchanged), `str title` (≤ 256), `u32 count` (≤ 50000) |
//! | node   | `u64 id` (1..2^62), `u32 flags` ([`flag`]), `u32 actions` ([`action`]), `i32 left, top, right, bottom` (display pixels), `str class_name` (≤ 256), `str text` (≤ 16384), `str content_description`, `str hint`, `str labeled_by` (the name of the node labelling this one) (each ≤ 4096), `str state_description`, `str pane_title`, `str tooltip` (each ≤ 1024), `str view_id` (≤ 256), `i32 selection_start, selection_end` (UTF-16 offsets, -1: none), `u8 live` (0 off, 1 polite, 2 assertive), range, collection, item, `u16 custom` (≤ 32) × (`i32 id`, `str label` ≤ 256), `u32 children` (≤ 10000) × `u64 id` |
//! | range  | `u8 kind` (0 none, 1 int, 2 float, 3 percent), then unless 0: `f32 min, max, current` |
//! | collection | `u8 present`, then if 1: `i32 rows, columns`, `u8 hierarchical`, `u8 selection_mode` |
//! | item   | `u8 present`, then if 1: `i32 row, column, row_span, column_span`, `u8 heading` |
//!
//! A full snapshot's first node is the window's root. It is a node of the
//! system app whose children are the roots of the display's Android windows.
//! A partial snapshot re-sends changed nodes with their children.

use crate::ipc::wire::{DecodeError, Reader};

pub const VERSION: u16 = 1;
pub const FLAG_PARTIAL: u16 = 1;

/// Node flags, one per boolean property of AccessibilityNodeInfo.
pub mod flag {
    pub const CHECKABLE: u32 = 1 << 0;
    pub const CHECKED: u32 = 1 << 1;
    pub const FOCUSABLE: u32 = 1 << 2;
    pub const FOCUSED: u32 = 1 << 3;
    pub const SELECTED: u32 = 1 << 4;
    pub const CLICKABLE: u32 = 1 << 5;
    pub const LONG_CLICKABLE: u32 = 1 << 6;
    pub const ENABLED: u32 = 1 << 7;
    pub const PASSWORD: u32 = 1 << 8;
    pub const SCROLLABLE: u32 = 1 << 9;
    pub const VISIBLE_TO_USER: u32 = 1 << 10;
    pub const EDITABLE: u32 = 1 << 11;
    pub const MULTI_LINE: u32 = 1 << 12;
    pub const CONTENT_INVALID: u32 = 1 << 13;
    pub const HEADING: u32 = 1 << 14;
    pub const SHOWING_HINT_TEXT: u32 = 1 << 15;
    pub const DISMISSABLE: u32 = 1 << 16;
    /// The whole text is one URLSpan, which Chromium calls a covering URL
    /// span.
    pub const TEXT_IS_LINK: u32 = 1 << 17;
    /// `isImportantForAccessibility`.
    pub const IMPORTANT: u32 = 1 << 18;
    /// The checked state is partial, from Android 16's tri-state
    /// `getChecked()`.
    pub const CHECKED_MIXED: u32 = 1 << 19;
}

/// Standard actions the node supports, one bit each. ACTION events name them
/// by bit index. [`CUSTOM`](action::CUSTOM) marks a custom action.
pub mod action {
    pub const CLICK: u8 = 0;
    pub const LONG_CLICK: u8 = 1;
    pub const FOCUS: u8 = 2;
    pub const CLEAR_FOCUS: u8 = 3;
    pub const SELECT: u8 = 4;
    pub const SCROLL_FORWARD: u8 = 5;
    pub const SCROLL_BACKWARD: u8 = 6;
    pub const SCROLL_UP: u8 = 7;
    pub const SCROLL_DOWN: u8 = 8;
    pub const SCROLL_LEFT: u8 = 9;
    pub const SCROLL_RIGHT: u8 = 10;
    pub const EXPAND: u8 = 11;
    pub const COLLAPSE: u8 = 12;
    pub const DISMISS: u8 = 13;
    pub const SET_TEXT: u8 = 14;
    pub const SET_SELECTION: u8 = 15;
    pub const SET_PROGRESS: u8 = 16;
    pub const SHOW_ON_SCREEN: u8 = 17;
    pub const CONTEXT_CLICK: u8 = 18;
    pub const SHOW_TOOLTIP: u8 = 19;
    pub const HIDE_TOOLTIP: u8 = 20;
    pub const CUSTOM: u8 = 31;

    pub const fn bit(a: u8) -> u32 {
        1 << a
    }
}

pub mod limits {
    pub const NODES: usize = 50_000;
    pub const CHILDREN: usize = 10_000;
    pub const CUSTOM_ACTIONS: usize = 32;
    pub const TITLE: usize = 256;
    pub const CLASS: usize = 256;
    pub const TEXT: usize = 16384;
    pub const LONG: usize = 4096;
    pub const SHORT: usize = 1024;
    /// Node ids at or above this belong to the bridge, which uses them for
    /// text runs.
    pub const MAX_ID: u64 = 1 << 62;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Range {
    pub kind: u8,
    pub min: f32,
    pub max: f32,
    pub current: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Collection {
    pub rows: i32,
    pub columns: i32,
    pub hierarchical: bool,
    pub selection_mode: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    pub row: i32,
    pub column: i32,
    pub row_span: i32,
    pub column_span: i32,
    pub heading: bool,
}

/// One AccessibilityNodeInfo as the system app sent it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RawNode {
    pub id: u64,
    pub flags: u32,
    pub actions: u32,
    /// left, top, right, bottom in display pixels.
    pub bounds: [i32; 4],
    pub class_name: String,
    pub text: String,
    pub content_description: String,
    pub hint: String,
    pub labeled_by: String,
    pub state_description: String,
    pub pane_title: String,
    pub tooltip: String,
    pub view_id: String,
    pub selection: (i32, i32),
    pub live: u8,
    pub range: Option<Range>,
    pub collection: Option<Collection>,
    pub item: Option<Item>,
    pub custom_actions: Vec<(i32, String)>,
    pub children: Vec<u64>,
}

impl RawNode {
    pub fn has(&self, f: u32) -> bool {
        self.flags & f != 0
    }
    pub fn can(&self, a: u8) -> bool {
        self.actions & action::bit(a) != 0
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Snapshot {
    pub partial: bool,
    pub scale: f32,
    pub focus: u64,
    pub title: String,
    pub nodes: Vec<RawNode>,
}

fn string(r: &mut Reader, max: usize) -> Result<String, DecodeError> {
    let n = r.u16()? as usize;
    if n > max {
        return Err(DecodeError::Invalid("string too long"));
    }
    let s = std::str::from_utf8(r.take(n)?).map_err(|_| DecodeError::Invalid("not UTF-8"))?;
    // D-Bus strings can't hold NUL.
    if s.contains('\0') {
        return Err(DecodeError::Invalid("NUL in string"));
    }
    Ok(s.to_owned())
}

fn id(r: &mut Reader) -> Result<u64, DecodeError> {
    let v = r.u64()?;
    if v == 0 || v >= limits::MAX_ID {
        return Err(DecodeError::Invalid("node id"));
    }
    Ok(v)
}

fn finite(r: &mut Reader) -> Result<f32, DecodeError> {
    let v = r.f32()?;
    if v.is_finite() {
        Ok(v)
    } else {
        Err(DecodeError::Invalid("not finite"))
    }
}

fn node(r: &mut Reader) -> Result<RawNode, DecodeError> {
    let mut n = RawNode {
        id: id(r)?,
        flags: r.u32()?,
        actions: r.u32()?,
        bounds: [r.i32()?, r.i32()?, r.i32()?, r.i32()?],
        class_name: string(r, limits::CLASS)?,
        text: string(r, limits::TEXT)?,
        content_description: string(r, limits::LONG)?,
        hint: string(r, limits::LONG)?,
        labeled_by: string(r, limits::LONG)?,
        state_description: string(r, limits::SHORT)?,
        pane_title: string(r, limits::SHORT)?,
        tooltip: string(r, limits::SHORT)?,
        view_id: string(r, limits::CLASS)?,
        selection: (r.i32()?, r.i32()?),
        ..Default::default()
    };
    n.live = r.u8()?;
    if n.live > 2 {
        return Err(DecodeError::Invalid("live"));
    }
    n.range = match r.u8()? {
        0 => None,
        kind @ 1..=3 => Some(Range {
            kind,
            min: finite(r)?,
            max: finite(r)?,
            current: finite(r)?,
        }),
        _ => return Err(DecodeError::Invalid("range kind")),
    };
    if r.bool()? {
        n.collection = Some(Collection {
            rows: r.i32()?,
            columns: r.i32()?,
            hierarchical: r.bool()?,
            selection_mode: r.u8()?,
        });
    }
    if r.bool()? {
        n.item = Some(Item {
            row: r.i32()?,
            column: r.i32()?,
            row_span: r.i32()?,
            column_span: r.i32()?,
            heading: r.bool()?,
        });
    }
    let custom = r.u16()? as usize;
    if custom > limits::CUSTOM_ACTIONS {
        return Err(DecodeError::Invalid("custom actions"));
    }
    for _ in 0..custom {
        n.custom_actions.push((r.i32()?, string(r, limits::CLASS)?));
    }
    let children = r.u32()? as usize;
    if children > limits::CHILDREN {
        return Err(DecodeError::Invalid("children"));
    }
    // The reservation is bounded by what the blob holds because the
    // declared count is untrusted.
    n.children.reserve(children.min(r.remaining() / 8));
    for _ in 0..children {
        n.children.push(id(r)?);
    }
    Ok(n)
}

pub fn decode(b: &[u8]) -> Result<Snapshot, DecodeError> {
    let mut r = Reader::new(b);
    if r.u16()? != VERSION {
        return Err(DecodeError::Invalid("version"));
    }
    let flags = r.u16()?;
    if flags & !FLAG_PARTIAL != 0 {
        return Err(DecodeError::Invalid("flags"));
    }
    let scale = finite(&mut r)?;
    if !(0.25..=16.0).contains(&scale) {
        return Err(DecodeError::Invalid("scale"));
    }
    let focus = r.u64()?;
    if focus >= limits::MAX_ID {
        return Err(DecodeError::Invalid("focus"));
    }
    let title = string(&mut r, limits::TITLE)?;
    let count = r.u32()? as usize;
    if count > limits::NODES || (count == 0 && flags & FLAG_PARTIAL == 0) {
        return Err(DecodeError::Invalid("count"));
    }
    let mut nodes = Vec::with_capacity(count.min(r.remaining() / 64));
    for _ in 0..count {
        nodes.push(node(&mut r)?);
    }
    r.end()?;
    Ok(Snapshot {
        partial: flags & FLAG_PARTIAL != 0,
        scale,
        focus,
        title,
        nodes,
    })
}

/// The encoder matching [`decode`]. Tests use it, and it produces the
/// fixture the system app's encoder is checked against.
pub fn encode(s: &Snapshot) -> Vec<u8> {
    use crate::ipc::wire::Writer;
    fn string(w: &mut Writer, s: &str) {
        w.u16(s.len() as u16);
        w.0.extend_from_slice(s.as_bytes());
    }
    let mut w = Writer::default();
    w.u16(VERSION)
        .u16(if s.partial { FLAG_PARTIAL } else { 0 })
        .f32(s.scale)
        .u64(s.focus);
    string(&mut w, &s.title);
    w.u32(s.nodes.len() as u32);
    for n in &s.nodes {
        w.u64(n.id).u32(n.flags).u32(n.actions);
        for v in n.bounds {
            w.i32(v);
        }
        for t in [
            &n.class_name,
            &n.text,
            &n.content_description,
            &n.hint,
            &n.labeled_by,
            &n.state_description,
            &n.pane_title,
            &n.tooltip,
            &n.view_id,
        ] {
            string(&mut w, t);
        }
        w.i32(n.selection.0).i32(n.selection.1).u8(n.live);
        match n.range {
            None => _ = w.u8(0),
            Some(g) => _ = w.u8(g.kind).f32(g.min).f32(g.max).f32(g.current),
        }
        match n.collection {
            None => _ = w.u8(0),
            Some(c) => {
                w.u8(1)
                    .i32(c.rows)
                    .i32(c.columns)
                    .u8(c.hierarchical as u8)
                    .u8(c.selection_mode);
            }
        }
        match n.item {
            None => _ = w.u8(0),
            Some(i) => {
                w.u8(1)
                    .i32(i.row)
                    .i32(i.column)
                    .i32(i.row_span)
                    .i32(i.column_span)
                    .u8(i.heading as u8);
            }
        }
        w.u16(n.custom_actions.len() as u16);
        for (id, label) in &n.custom_actions {
            w.i32(*id);
            string(&mut w, label);
        }
        w.u32(n.children.len() as u32);
        for c in &n.children {
            w.u64(*c);
        }
    }
    w.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// The snapshot that `snapshot-fixture.txt` describes. The system app's
    /// encoder (`A11ySnapshotTest`) must produce it byte for byte.
    pub(crate) fn fixture() -> Snapshot {
        Snapshot {
            partial: false,
            scale: 1.5,
            focus: 3,
            title: "Probes".into(),
            nodes: vec![
                RawNode {
                    id: 1,
                    bounds: [0, 0, 1200, 900],
                    children: vec![2],
                    selection: (-1, -1),
                    ..Default::default()
                },
                RawNode {
                    id: 2,
                    flags: flag::ENABLED | flag::VISIBLE_TO_USER,
                    actions: action::bit(action::SCROLL_FORWARD),
                    bounds: [0, 0, 1200, 900],
                    class_name: "android.widget.ListView".into(),
                    selection: (-1, -1),
                    collection: Some(Collection {
                        rows: 2,
                        columns: 1,
                        hierarchical: false,
                        selection_mode: 0,
                    }),
                    children: vec![3],
                    ..Default::default()
                },
                RawNode {
                    id: 3,
                    flags: flag::ENABLED | flag::EDITABLE | flag::FOCUSED,
                    actions: action::bit(action::SET_TEXT) | action::bit(action::CLICK),
                    bounds: [10, 20, 300, 80],
                    class_name: "android.widget.EditText".into(),
                    text: "héllo".into(),
                    hint: "Name".into(),
                    view_id: "dev.sargunv.innkeeper.probes:id/name".into(),
                    selection: (1, 3),
                    live: 1,
                    range: Some(Range {
                        kind: 2,
                        min: 0.0,
                        max: 10.0,
                        current: 2.5,
                    }),
                    item: Some(Item {
                        row: 0,
                        column: 0,
                        row_span: 1,
                        column_span: 1,
                        heading: false,
                    }),
                    custom_actions: vec![(7, "Archive".into())],
                    ..Default::default()
                },
            ],
        }
    }

    #[test]
    fn shared_fixture() {
        let text = include_str!("snapshot-fixture.txt");
        let hexes: String = text
            .lines()
            .filter(|l| !l.trim_start().starts_with('#'))
            .collect();
        let want = hex(&hexes);
        assert_eq!(encode(&fixture()), want);
        assert_eq!(decode(&want), Ok(fixture()));
    }

    #[test]
    fn refuses_bad_snapshots() {
        let good = encode(&fixture());
        assert!(decode(&good[..good.len() - 1]).is_err());
        let mut long = good.clone();
        long.push(0);
        assert!(decode(&long).is_err());
        let mut s = fixture();
        s.nodes[1].id = 0;
        assert!(decode(&encode(&s)).is_err());
        let mut s = fixture();
        s.nodes[1].children.push(limits::MAX_ID);
        assert!(decode(&encode(&s)).is_err());
        let mut s = fixture();
        s.scale = 0.0;
        assert!(decode(&encode(&s)).is_err());
        let mut s = fixture();
        s.nodes[2].text = "a\0b".into();
        assert!(decode(&encode(&s)).is_err());
        let mut s = fixture();
        s.nodes[2].range.as_mut().unwrap().max = f32::NAN;
        assert!(decode(&encode(&s)).is_err());
        let mut s = fixture();
        s.nodes.clear();
        assert!(decode(&encode(&s)).is_err(), "a full snapshot needs a root");
        s.partial = true;
        assert!(decode(&encode(&s)).is_ok());
    }
}
