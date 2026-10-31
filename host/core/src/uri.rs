//! `file://` URIs, as desktop launches and the FileChooser portal pass files.

use std::path::PathBuf;

/// A local path from a `file://` URI (percent-decoded), or None.
pub fn file_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    if !rest.starts_with('/') {
        return None;
    }
    let b = rest.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    if out.contains(&0) {
        return None;
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris() {
        assert_eq!(
            file_path("file:///home/u/a%20b.txt"),
            Some("/home/u/a b.txt".into())
        );
        assert_eq!(file_path("file://localhost/x"), Some("/x".into()));
        assert_eq!(file_path("file://host/x"), None);
        assert_eq!(file_path("https://x/y"), None);
        assert_eq!(file_path("file:///a%00b"), None);
        assert_eq!(file_path("file:///a%2"), None);
    }
}
