//! The text-input line protocol between the Innkeeper IME (an
//! `InputMethodService` in the system app) and the seat's `zwp_text_input_v3`
//! (`text_input.rs`).
//!
//! The IME's lines arrive as input requests (`TEXT_INPUT`); the bridge's go
//! back as ordinary input lines (`INPUT` events), so they stay ordered with
//! the keys. Everything here is pure and tested; parsing is strict and
//! canonical.
//!
//! | IME line                                         | Effect                                                                                                         |
//! | ------------------------------------------------ | -------------------------------------------------------------------------------------------------------------- |
//! | `ti_enable` / `ti_disable`                       | `enable` / `disable`                                                                                           |
//! | `ti_content <hint> <purpose>`                    | `set_content_type` (hint ≤ `0x3ff`, purpose ≤ 13)                                                              |
//! | `ti_surrounding <cursor> <anchor> <0\|1> <text>` | `set_surrounding_text`; byte offsets on char boundaries; 1 also sets the change cause to `other`               |
//! | `ti_cursor <x> <y> <w> <h>`                      | `set_cursor_rectangle`, from the display's pixels divided by the window's scale                                |
//! | `ti_apply`                                       | `commit`                                                                                                       |
//!
//! At each `done` the IME gets `ti_delete <before> <after>` (bytes),
//! `ti_commit <text>` (repeated for long text; the IME appends),
//! `ti_preedit <begin> <end> [<text>]` (always; no text means no preedit),
//! then `ti_done <window>`. On `leave` it gets an empty preedit and
//! `ti_done`, as the protocol asks.
//!
//! Text is percent-encoded: bytes up to space, DEL and `%` become `%XX`;
//! everything else (including non-ASCII UTF-8) is sent as is.

/// Longest line the IME may send.
pub const MAX_LINE: usize = 480;
/// Longest text chunk the bridge puts in one line (input lines are at most
/// 400 bytes).
pub const CHUNK: usize = 320;
/// Largest cursor-rectangle coordinate, in display pixels.
const MAX_COORD: i32 = 1 << 20;

/// zwp_text_input_v3 content hints and purposes we accept (the protocol's
/// whole range in version 1).
pub const MAX_HINT: u32 = 0x3ff;
pub const MAX_PURPOSE: u32 = 13;

/// A request from the IME.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Req {
    /// An editor took focus (a new input): enable text input.
    Enable,
    /// No editor any more.
    Disable,
    /// zwp_text_input_v3 content hint bits and purpose.
    Content { hint: u32, purpose: u32 },
    /// Text around the cursor; `cursor` and `anchor` are byte offsets into
    /// `text`, on char boundaries. `other`: the change didn't come from the
    /// input method (the user clicked elsewhere, the app changed the text).
    Surrounding {
        text: String,
        cursor: u32,
        anchor: u32,
        other: bool,
    },
    /// The text cursor, in the display's pixels (the bridge divides by the
    /// window's scale).
    Cursor { x: i32, y: i32, w: i32, h: i32 },
    /// Commits the state above (`zwp_text_input_v3.commit`).
    Apply,
}

impl Req {
    pub fn to_line(&self) -> String {
        match self {
            Req::Enable => "ti_enable".into(),
            Req::Disable => "ti_disable".into(),
            Req::Content { hint, purpose } => format!("ti_content {hint} {purpose}"),
            Req::Surrounding {
                text,
                cursor,
                anchor,
                other,
            } => format!(
                "ti_surrounding {cursor} {anchor} {} {}",
                *other as u8,
                encode(text)
            ),
            Req::Cursor { x, y, w, h } => format!("ti_cursor {x} {y} {w} {h}"),
            Req::Apply => "ti_apply".into(),
        }
    }
}

/// Parses one IME line. Only the canonical form is accepted (it re-encodes
/// to the same bytes).
pub fn parse(line: &str) -> Option<Req> {
    if line.len() > MAX_LINE {
        return None;
    }
    let mut it = line.split(' ');
    let op = it.next()?;
    let mut num = || -> Option<i64> { it.next()?.parse::<i64>().ok() };
    let req = match op {
        "ti_enable" => Req::Enable,
        "ti_disable" => Req::Disable,
        "ti_apply" => Req::Apply,
        "ti_content" => {
            let hint = u32::try_from(num()?).ok().filter(|h| *h <= MAX_HINT)?;
            let purpose = u32::try_from(num()?).ok().filter(|p| *p <= MAX_PURPOSE)?;
            Req::Content { hint, purpose }
        }
        "ti_cursor" => {
            let mut c = [0i32; 4];
            for v in &mut c {
                *v = i32::try_from(num()?).ok()?;
            }
            let [x, y, w, h] = c;
            let ok = |v: i32| (-MAX_COORD..=MAX_COORD).contains(&v);
            if !(ok(x) && ok(y) && (0..=MAX_COORD).contains(&w) && (0..=MAX_COORD).contains(&h)) {
                return None;
            }
            Req::Cursor { x, y, w, h }
        }
        "ti_surrounding" => {
            let cursor = u32::try_from(num()?).ok()?;
            let anchor = u32::try_from(num()?).ok()?;
            let other = match num()? {
                0 => false,
                1 => true,
                _ => return None,
            };
            let text = decode(it.next().unwrap_or(""))?;
            let ok = |o: u32| text.is_char_boundary(o as usize);
            if !ok(cursor) || !ok(anchor) {
                return None;
            }
            Req::Surrounding {
                text,
                cursor,
                anchor,
                other,
            }
        }
        _ => return None,
    };
    if it.next().is_some() {
        return None;
    }
    (req.to_line() == line).then_some(req)
}

/// What the compositor asked for in one `done` (zwp_text_input_v3 order:
/// remove the preedit, delete around the cursor, insert the commit, show the
/// new preedit).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Done {
    /// Bytes to delete before and after the cursor (UTF-8 lengths, relative
    /// to the surrounding text the IME sent).
    pub delete: Option<(u32, u32)>,
    pub commit: Option<String>,
    /// Preedit text and its cursor (byte offsets; -1 hides the cursor).
    pub preedit: Option<(String, i32, i32)>,
}

impl Done {
    /// The lines that carry this to the IME, ending with `ti_done <window>`.
    /// Commit text is split over `ti_commit` lines (the IME appends them);
    /// a preedit longer than one chunk is truncated.
    pub fn to_lines(&self, window: u32) -> Vec<String> {
        let mut out = Vec::new();
        if let Some((b, a)) = self.delete {
            out.push(format!("ti_delete {b} {a}"));
        }
        if let Some(c) = &self.commit {
            for chunk in chunks(c, CHUNK) {
                out.push(format!("ti_commit {chunk}"));
            }
        }
        match &self.preedit {
            Some((t, b, e)) if !t.is_empty() => {
                let t = encode_prefix(t, CHUNK);
                let n = decode(&t).map_or(0, |s| s.len()) as i32;
                let (b, e) = if *b < 0 || *e < 0 {
                    (-1, -1)
                } else {
                    ((*b).min(n), (*e).min(n))
                };
                out.push(format!("ti_preedit {b} {e} {t}"));
            }
            _ => out.push("ti_preedit -1 -1".into()),
        }
        out.push(format!("ti_done {window}"));
        out
    }
}

fn escaped(b: u8) -> bool {
    b <= b' ' || b == 0x7f || b == b'%'
}

pub fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        push_char(&mut out, ch);
    }
    out
}

fn push_char(out: &mut String, ch: char) {
    if ch.is_ascii() && escaped(ch as u8) {
        out.push_str(&format!("%{:02X}", ch as u8));
    } else {
        out.push(ch);
    }
}

/// The longest encoded prefix of `s` that fits in `max` bytes.
fn encode_prefix(s: &str, max: usize) -> String {
    chunks(s, max).next().unwrap_or_default()
}

/// `s` encoded and split into pieces of at most `max` bytes, never inside a
/// character or an escape.
fn chunks(s: &str, max: usize) -> impl Iterator<Item = String> + '_ {
    let mut chars = s.chars().peekable();
    std::iter::from_fn(move || {
        chars.peek()?;
        let mut out = String::new();
        while let Some(&ch) = chars.peek() {
            let mut one = String::new();
            push_char(&mut one, ch);
            if !out.is_empty() && out.len() + one.len() > max {
                break;
            }
            out.push_str(&one);
            chars.next();
        }
        Some(out)
    })
}

/// Decodes percent-encoded text; fails on bad escapes, on bytes that should
/// have been escaped, on escapes that needn't be, and on invalid UTF-8.
pub fn decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let hex = s.get(i + 1..i + 3)?;
                if !hex
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'A'..=b'F').contains(&c))
                {
                    return None;
                }
                let v = u8::from_str_radix(hex, 16).ok()?;
                if !escaped(v) {
                    return None;
                }
                out.push(v);
                i += 3;
            }
            c if escaped(c) => return None,
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    let text = String::from_utf8(out).ok()?;
    // NUL would end C strings on the compositor side.
    (!text.contains('\0')).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_roundtrip() {
        for r in [
            Req::Enable,
            Req::Disable,
            Req::Apply,
            Req::Content {
                hint: 0x204,
                purpose: 6,
            },
            Req::Cursor {
                x: -3,
                y: 40,
                w: 2,
                h: 30,
            },
            Req::Surrounding {
                text: "héllo 100% wörld\n日本".into(),
                cursor: 5,
                anchor: 1,
                other: true,
            },
            Req::Surrounding {
                text: String::new(),
                cursor: 0,
                anchor: 0,
                other: false,
            },
        ] {
            assert_eq!(parse(&r.to_line()), Some(r));
        }
    }

    #[test]
    fn rejects() {
        for l in [
            "",
            "ti_enable ",
            "ti_enable 1",
            "ti_content 1",
            "ti_content 0x1 0",
            "ti_content 1024 0",
            "ti_content 1 14",
            "ti_content +1 1",
            "ti_content 01 1",
            "ti_cursor 0 0 -1 1",
            "ti_cursor 0 0 1 1 1",
            "ti_cursor 9999999999 0 1 1",
            "ti_surrounding 2 0 0 h%C3%A9", // escaped non-ASCII
            "ti_surrounding 2 0 0 hé",      // cursor inside 'é'
            "ti_surrounding 0 0 0 a b",
            "ti_surrounding 0 0 2 a",
            "ti_surrounding 0 0 a",
            "ti_surrounding 0 0 0 a%2",
            "ti_surrounding 0 0 0 a%2b",
            "ti_surrounding 0 0 0 %00",
            "ti_surrounding 0 0 0 %41",
            "ti_surrounding 9 0 0 abc",
            "ti_bogus",
        ] {
            assert_eq!(parse(l), None, "{l:?}");
        }
        assert_eq!(
            parse(&format!("ti_surrounding 0 0 0 {}", "a".repeat(500))),
            None
        );
    }

    #[test]
    fn done_lines() {
        let d = Done {
            delete: Some((3, 0)),
            commit: Some("你好 world".into()),
            preedit: Some(("ni h".into(), 4, 4)),
        };
        assert_eq!(
            d.to_lines(7),
            [
                "ti_delete 3 0",
                "ti_commit 你好%20world",
                "ti_preedit 4 4 ni%20h",
                "ti_done 7"
            ]
        );
        assert_eq!(
            Done::default().to_lines(2),
            ["ti_preedit -1 -1", "ti_done 2"]
        );
        // A long commit is split; every line fits an input line.
        let long = "é".repeat(400);
        let lines = Done {
            commit: Some(long.clone()),
            ..Default::default()
        }
        .to_lines(1);
        let joined: String = lines
            .iter()
            .filter_map(|l| l.strip_prefix("ti_commit "))
            .map(|c| decode(c).unwrap())
            .collect();
        assert_eq!(joined, long);
        assert!(lines.iter().all(|l| l.len() <= 400));
        // A long preedit is truncated, and its cursor clamped.
        let p = Done {
            preedit: Some(("%".repeat(500), 450, 500)),
            ..Default::default()
        }
        .to_lines(1);
        assert!(p[0].len() <= 400);
        assert!(p[0].starts_with("ti_preedit 106 106 %25"));
    }
}
