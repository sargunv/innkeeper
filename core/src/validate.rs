//! Validation of strings that come from untrusted APKs (or the desktop) before
//! they reach paths, Android commands or `.desktop` files.

use anyhow::{Result, bail};

/// Android's package-name grammar (`PackageParser.validateName` with a
/// required separator): two or more dot-separated segments, each a letter
/// followed by letters, digits or `_`; at most 255 bytes. Such a name is also a
/// valid file name and a valid desktop-file ID element.
pub fn package(name: &str) -> Result<&str> {
    let ok = !name.is_empty()
        && name.len() <= 255
        && name.contains('.')
        && name.split('.').all(|seg| {
            let mut c = seg.chars();
            c.next().is_some_and(|f| f.is_ascii_alphabetic())
                && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        });
    if !ok {
        bail!("not a valid Android package name: {:?}", truncate(name, 80));
    }
    Ok(name)
}

/// A URI scheme we may declare as `x-scheme-handler/<scheme>`: RFC 3986
/// syntax, lowercase only (desktop MIME matching is case-insensitive, Android's
/// scheme matching is not), and never one that would capture generic
/// navigation or local content.
pub fn custom_scheme(s: &str) -> bool {
    const DENY: &[&str] = &[
        "http",
        "https",
        "file",
        "content",
        "android.resource",
        "android-app",
        "intent",
        "javascript",
        "data",
        "about",
        "blob",
        "ftp",
        "ws",
        "wss",
    ];
    let mut c = s.chars();
    s.len() <= 64
        && c.next().is_some_and(|f| f.is_ascii_lowercase())
        && c.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || "+-.".contains(ch))
        && !DENY.contains(&s)
}

/// A host pattern from an `http(s)` intent filter (`example.org`,
/// `*.example.org`). Only recorded as metadata, never registered.
pub fn web_host(s: &str) -> bool {
    let h = s.strip_prefix("*.").unwrap_or(s);
    !h.is_empty()
        && h.len() <= 253
        && !h.starts_with(['.', '-'])
        && h.chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ".-".contains(ch))
}

/// Characters that must never reach a desktop entry or terminal: C0/C1
/// controls, DEL, and the bidi embedding/override/isolate controls (which can
/// make a label render as something else).
pub fn is_forbidden_char(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{FEFF}')
}

/// A display label (app name): trimmed, 1–128 chars, no forbidden characters.
/// Returns `None` if the label must be rejected.
pub fn label(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() || t.chars().count() > 128 || t.chars().any(is_forbidden_char) {
        return None;
    }
    Some(t.to_string())
}

/// A locale tag from aapt2 (`de`, `pt-BR`) as a desktop-entry locale
/// (`de`, `pt_BR`). Other forms (`b+sr+Latn`) are skipped.
pub fn desktop_locale(tag: &str) -> Option<String> {
    let (lang, region) = match tag.split_once('-') {
        Some((l, r)) => (l, Some(r)),
        None => (tag, None),
    };
    let lang_ok = (2..=3).contains(&lang.len()) && lang.chars().all(|c| c.is_ascii_lowercase());
    let region_ok =
        region.is_none_or(|r| r.len() == 2 && r.chars().all(|c| c.is_ascii_uppercase()));
    if !lang_ok || !region_ok {
        return None;
    }
    // Android still uses the legacy codes for these.
    let lang = match lang {
        "in" => "id",
        "iw" => "he",
        "ji" => "yi",
        l => l,
    };
    Some(match region {
        Some(r) => format!("{lang}_{r}"),
        None => lang.to_string(),
    })
}

/// A URI handed to `linuxapk run` (from `%U`, so possibly from a web page):
/// must have a scheme, no forbidden characters, and not be a local file (the
/// session can't see host files).
pub fn launch_uri(u: &str) -> Result<&str> {
    if u.len() > 8192 || u.chars().any(|c| is_forbidden_char(c) || c == ' ') {
        bail!("URI contains spaces or control characters");
    }
    let Some((scheme, _)) = u.split_once(':') else {
        bail!("not a URI (no scheme): {:?}", truncate(u, 80));
    };
    let mut c = scheme.chars();
    let ok = c.next().is_some_and(|f| f.is_ascii_alphabetic())
        && c.all(|ch| ch.is_ascii_alphanumeric() || "+-.".contains(ch));
    if !ok {
        bail!("not a URI (bad scheme): {:?}", truncate(u, 80));
    }
    let lower = scheme.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "file" | "content" | "intent" | "javascript" | "android.resource"
    ) {
        bail!("{lower}: URIs can't be opened in Android apps");
    }
    Ok(u)
}

/// For error messages: at most `n` chars, with forbidden characters escaped.
pub fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s
        .chars()
        .take(n)
        .map(|c| if is_forbidden_char(c) { '\u{FFFD}' } else { c })
        .collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_names() {
        for ok in [
            "org.wikipedia",
            "com.darkempire78.opencalculator",
            "a.b",
            "A_1.b_2",
        ] {
            assert!(package(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "wikipedia",
            "org..wiki",
            ".org.wiki",
            "org.wiki.",
            "org.1wiki",
            "org._wiki",
            "org.wi-ki",
            "org.wiki/../x",
            "org.wiki\n",
            "org.wïki",
            "org wiki.x",
            &format!("a.{}", "b".repeat(254)),
        ] {
            assert!(package(bad).is_err(), "{bad:?}");
        }
        assert!(package(&format!("a.{}", "b".repeat(253))).is_ok());
    }

    #[test]
    fn schemes() {
        for ok in ["wikipedia", "mailto", "geo", "my-app+x.y", "x1"] {
            assert!(custom_scheme(ok), "{ok}");
        }
        for bad in [
            "http",
            "https",
            "file",
            "content",
            "intent",
            "javascript",
            "",
            "1abc",
            "Wikipedia",
            "a b",
            "a;b",
            "a/b",
            "a\nb",
            "ws",
        ] {
            assert!(!custom_scheme(bad), "{bad:?}");
        }
        assert!(!custom_scheme(&"a".repeat(65)));
    }

    #[test]
    fn hosts() {
        assert!(web_host("*.wikipedia.org"));
        assert!(web_host("en.m.wikipedia.org"));
        assert!(!web_host("*"));
        assert!(!web_host("evil;x"));
        assert!(!web_host(".x.org"));
        assert!(!web_host("A.org"));
    }

    #[test]
    fn labels() {
        assert_eq!(label("  Calculator ").as_deref(), Some("Calculator"));
        assert_eq!(label("الحاسبة").as_deref(), Some("الحاسبة"));
        assert_eq!(
            label("Fish & Chips; \"quoted\" \\").as_deref(),
            Some("Fish & Chips; \"quoted\" \\")
        );
        assert_eq!(label(""), None);
        assert_eq!(label("a\nExec=evil"), None);
        assert_eq!(label("a\rb"), None);
        assert_eq!(label("a\u{7f}"), None);
        assert_eq!(label("a\u{85}b"), None);
        assert_eq!(label("gpj.\u{202E}exe"), None);
        assert_eq!(label(&"x".repeat(129)), None);
    }

    #[test]
    fn locales() {
        assert_eq!(desktop_locale("de").as_deref(), Some("de"));
        assert_eq!(desktop_locale("pt-BR").as_deref(), Some("pt_BR"));
        assert_eq!(desktop_locale("in").as_deref(), Some("id"));
        assert_eq!(desktop_locale("b+sr+Latn"), None);
        assert_eq!(desktop_locale("zh-rTW"), None);
        assert_eq!(desktop_locale("de]=x"), None);
    }

    #[test]
    fn uris() {
        assert!(launch_uri("https://en.wikipedia.org/wiki/Linux").is_ok());
        assert!(launch_uri("wikipedia://en.wikipedia.org/wiki/X").is_ok());
        assert!(launch_uri("mailto:a@b.c").is_ok());
        assert!(launch_uri("file:///etc/passwd").is_err());
        assert!(launch_uri("FILE:///etc/passwd").is_err());
        assert!(launch_uri("intent:#Intent;end").is_err());
        assert!(launch_uri("--es x y").is_err());
        assert!(launch_uri("-n:x").is_err());
        assert!(launch_uri("https://x\n").is_err());
        assert!(launch_uri("nocolon").is_err());
    }
}
