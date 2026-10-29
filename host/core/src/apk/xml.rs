//! Android binary XML (`ResXMLTree`), the compiled form of an APK's
//! `AndroidManifest.xml`.

use anyhow::{Context, Result, bail};

use super::chunk::{Chunk, STRING_POOL, StringPool, Value, u8_at, u16_at, u32_at};

const XML: u16 = 0x0003;
const RESOURCE_MAP: u16 = 0x0180;
const START_ELEMENT: u16 = 0x0102;
const END_ELEMENT: u16 = 0x0103;

/// Deeper nesting than any real manifest; bounds the tree.
const MAX_DEPTH: usize = 128;

/// `android:` attributes by resource ID. Android identifies framework
/// attributes by ID alone, so these names win over the string pool's (which
/// shrinkers may rename).
const ANDROID_ATTRS: &[(u32, &str)] = &[
    (0x01010199, "drawable"),
    (0x01010402, "viewportWidth"),
    (0x01010403, "viewportHeight"),
    (0x01010404, "fillColor"),
    (0x010104cc, "fillAlpha"),
    (0x01010406, "strokeColor"),
    (0x010104cb, "strokeAlpha"),
    (0x01010407, "strokeWidth"),
    (0x0101040b, "strokeLineCap"),
    (0x0101040c, "strokeLineJoin"),
    (0x0101040d, "strokeMiterLimit"),
    (0x01010408, "trimPathStart"),
    (0x01010409, "trimPathEnd"),
    (0x0101040a, "trimPathOffset"),
    (0x01010326, "rotation"),
    (0x010101b5, "pivotX"),
    (0x010101b6, "pivotY"),
    (0x01010324, "scaleX"),
    (0x01010325, "scaleY"),
    (0x0101045a, "translateX"),
    (0x0101045b, "translateY"),
    (0x0101031f, "alpha"),
    (0x01010121, "tint"),
    (0x0101051e, "fillType"),
    (0x01010405, "pathData"),
    (0x0101_0001, "label"),
    (0x0101_0002, "icon"),
    (0x0101_0003, "name"),
    (0x0101_0010, "exported"),
    (0x0101_0026, "mimeType"),
    (0x0101_0027, "scheme"),
    (0x0101_0028, "host"),
    (0x0101_021b, "versionCode"),
    (0x0101_021c, "versionName"),
    (0x0101_0271, "maxSdkVersion"),
    (0x0101_03f4, "isGame"),
    (0x0101_0545, "appCategory"),
];

/// An element, with attributes by local name (namespaces dropped).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attrs: Vec<(String, Value)>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn attr(&self, local: &str) -> Option<&Value> {
        self.attrs.iter().find(|(k, _)| k == local).map(|(_, v)| v)
    }

    pub fn find<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// Parses a binary XML document into its root element.
pub fn parse(b: &[u8]) -> Result<Element> {
    let doc = Chunk::at(b).context("binary XML")?;
    if doc.ty != XML {
        bail!("not binary XML (chunk type {:#06x})", doc.ty);
    }
    let mut pool = None;
    let mut ids: Vec<u32> = vec![];
    // The open elements; a synthetic root collects the document element.
    let mut stack = vec![Element::default()];
    for c in doc.children() {
        let c = c?;
        match c.ty {
            STRING_POOL if pool.is_none() => pool = Some(StringPool::new(c)?),
            RESOURCE_MAP => {
                ids = c
                    .body
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|w| u32::from_le_bytes(*w))
                    .collect();
            }
            START_ELEMENT => {
                let pool = pool
                    .as_ref()
                    .context("binary XML element before its string pool")?;
                if stack.len() > MAX_DEPTH {
                    bail!("binary XML nests deeper than {MAX_DEPTH}");
                }
                stack.push(element(c.body, pool, &ids)?);
            }
            END_ELEMENT => {
                if stack.len() < 2 {
                    bail!("binary XML closes more elements than it opens");
                }
                let e = stack.pop().unwrap();
                stack.last_mut().unwrap().children.push(e);
            }
            // Namespaces, text, comments.
            _ => {}
        }
    }
    if stack.len() != 1 {
        bail!("binary XML ends inside an element");
    }
    stack
        .pop()
        .unwrap()
        .children
        .into_iter()
        .next()
        .context("binary XML has no elements")
}

/// A `ResXMLTree_attrExt` and its attributes.
fn element(ext: &[u8], pool: &StringPool, ids: &[u32]) -> Result<Element> {
    let name = pool.get(u32_at(ext, 4)?)?;
    let start = u16_at(ext, 8)? as usize;
    let size = u16_at(ext, 10)? as usize;
    let count = u16_at(ext, 12)? as usize;
    if size < 20 {
        bail!("binary XML attribute size {size}");
    }
    let mut attrs = Vec::with_capacity(count);
    for i in 0..count {
        let a = ext
            .get(start + i * size..start + (i + 1) * size)
            .context("binary XML attribute past its element")?;
        let n = u32_at(a, 4)?;
        let known = ids
            .get(n as usize)
            .and_then(|id| ANDROID_ATTRS.iter().find(|(k, _)| k == id));
        let name = match known {
            Some((_, s)) => s.to_string(),
            None => pool.get(n)?,
        };
        let value = Value::new(u8_at(a, 15)?, u32_at(a, 16)?, pool)?;
        attrs.push((name, value));
    }
    Ok(Element {
        name,
        attrs,
        children: vec![],
    })
}

#[cfg(test)]
pub mod build {
    //! A writer for hand-made binary XML in tests.

    use super::super::chunk::build::{chunk, pool};

    /// An attribute value.
    pub enum V<'a> {
        S(&'a str),
        R(u32),
        I(u32),
        B(bool),
    }

    /// An element: name, attributes, children. Attribute names that are
    /// known `android:` attributes get their resource ID.
    pub struct Node<'a>(pub &'a str, pub Vec<(&'a str, V<'a>)>, pub Vec<Node<'a>>);

    fn collect<'a>(n: &Node<'a>, attrs: &mut Vec<&'a str>, strings: &mut Vec<&'a str>) {
        strings.push(n.0);
        for (k, v) in &n.1 {
            attrs.push(k);
            if let V::S(s) = v {
                strings.push(s);
            }
        }
        n.2.iter().for_each(|c| collect(c, attrs, strings));
    }

    pub fn document(root: &Node) -> Vec<u8> {
        let (mut attrs, mut rest) = (vec![], vec![]);
        collect(root, &mut attrs, &mut rest);
        // Attribute names with IDs first, as aapt2 lays them out.
        let mut strings: Vec<&str> = vec![];
        let mut ids = vec![];
        for (id, name) in super::ANDROID_ATTRS {
            if attrs.contains(name) {
                strings.push(name);
                ids.push(*id);
            }
        }
        for s in attrs.into_iter().chain(rest) {
            if !strings.contains(&s) {
                strings.push(s);
            }
        }
        let idx = |s: &str| strings.iter().position(|x| *x == s).unwrap() as u32;
        let mut body = pool(&strings, false);
        let map: Vec<u8> = ids.iter().flat_map(|i: &u32| i.to_le_bytes()).collect();
        body.extend(chunk(0x0180, &[], &map));
        fn emit(n: &Node, idx: &dyn Fn(&str) -> u32, out: &mut Vec<u8>) {
            let node_header = [1u32.to_le_bytes(), u32::MAX.to_le_bytes()].concat();
            let mut ext = vec![];
            ext.extend(u32::MAX.to_le_bytes());
            ext.extend(idx(n.0).to_le_bytes());
            ext.extend(20u16.to_le_bytes());
            ext.extend(20u16.to_le_bytes());
            ext.extend((n.1.len() as u16).to_le_bytes());
            ext.extend([0u8; 6]);
            for (name, v) in &n.1 {
                let (ty, data) = match v {
                    V::S(s) => (0x03, idx(s)),
                    V::R(r) => (0x01, *r),
                    V::I(i) => (0x10, *i),
                    V::B(b) => (0x12, if *b { u32::MAX } else { 0 }),
                };
                ext.extend(u32::MAX.to_le_bytes());
                ext.extend(idx(name).to_le_bytes());
                ext.extend(u32::MAX.to_le_bytes());
                ext.extend([8, 0, 0, ty]);
                ext.extend(data.to_le_bytes());
            }
            out.extend(chunk(0x0102, &node_header, &ext));
            for c in &n.2 {
                emit(c, idx, out);
            }
            let end = [u32::MAX.to_le_bytes(), idx(n.0).to_le_bytes()].concat();
            out.extend(chunk(0x0103, &node_header, &end));
        }
        emit(root, &idx, &mut body);
        chunk(0x0003, &[], &body)
    }
}

#[cfg(test)]
mod tests {
    use super::build::{Node, V, document};
    use super::*;

    #[test]
    fn tree() {
        let doc = document(&Node(
            "manifest",
            vec![("package", V::S("a.b"))],
            vec![Node(
                "application",
                vec![
                    ("label", V::R(0x7f01_0002)),
                    ("on", V::B(true)),
                    ("name", V::I(7)),
                ],
                vec![],
            )],
        ));
        let root = parse(&doc).unwrap();
        assert_eq!(root.name, "manifest");
        assert_eq!(root.attr("package"), Some(&Value::Str("a.b".into())));
        let app = root.find("application").next().unwrap();
        assert_eq!(app.attr("label"), Some(&Value::Ref(0x7f01_0002)));
        assert_eq!(app.attr("on"), Some(&Value::Bool(true)));
        assert_eq!(app.attr("name"), Some(&Value::Int(7)));
        assert_eq!(app.attr("x"), None);
    }

    /// Android goes by an `android:` attribute's ID, not its pool name.
    #[test]
    fn attribute_ids_win() {
        let mut doc = document(&Node("manifest", vec![("label", V::S("x"))], vec![]));
        // The pool is UTF-16 and "label" its first string: rename it "LABEL".
        let at = doc
            .windows(10)
            .position(|w| {
                w == "label"
                    .encode_utf16()
                    .flat_map(u16::to_le_bytes)
                    .collect::<Vec<_>>()
            })
            .unwrap();
        for (i, u) in "LABEL".encode_utf16().enumerate() {
            doc[at + 2 * i..at + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
        }
        let root = parse(&doc).unwrap();
        assert_eq!(root.attr("label"), Some(&Value::Str("x".into())));
    }

    #[test]
    fn malformed() {
        let doc = document(&Node("manifest", vec![("package", V::S("a"))], vec![]));
        // Truncated anywhere: an error, never a panic.
        for n in 0..doc.len() {
            let mut d = doc[..n].to_vec();
            if d.len() >= 8 {
                d[4..8].copy_from_slice(&(n as u32).to_le_bytes());
            }
            assert!(parse(&d).is_err(), "truncated at {n}");
        }
        let mut deep = Node("manifest", vec![], vec![]);
        for _ in 0..200 {
            deep = Node("manifest", vec![], vec![deep]);
        }
        assert!(parse(&document(&deep)).is_err());
    }
}
