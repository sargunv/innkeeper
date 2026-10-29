//! Opening links from Android through the OpenURI
//! portal. Absolute external URIs in
//! plain ASCII get through, at most [`MAX_URI`] bytes and [`RATE`] per
//! window per connection; every one is logged.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use zbus::zvariant::Value;

use crate::ipc::wire::errno::*;

pub const MAX_URI: usize = 8192;
/// At most this many opens per connection in any [`RATE_WINDOW`].
pub const RATE: usize = 5;
pub const RATE_WINDOW: Duration = Duration::from_secs(10);

/// Checks that `u` is an external URI (http(s) must have a host), in printable
/// ASCII without spaces, quotes or backslashes (Android sends it
/// percent-encoded; IRIs are converted on that side).
pub fn validate(u: &str) -> Result<(), &'static str> {
    if u.is_empty() || u.len() > MAX_URI {
        return Err("length");
    }
    if !u
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-._~:/?#[]@!$&'()*+,;=%".contains(&b))
    {
        return Err("character not allowed in a URL");
    }
    let lower = u.to_ascii_lowercase();
    let (scheme, value) = lower.split_once(':').ok_or("no scheme")?;
    if scheme.is_empty()
        || !scheme.as_bytes()[0].is_ascii_alphabetic()
        || !scheme
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+-.".contains(&b))
        || matches!(
            scheme,
            "file" | "content" | "intent" | "javascript" | "data"
        )
        || (value.is_empty() && scheme != "mailto")
    {
        return Err("not an external URI");
    }
    if !matches!(scheme, "http" | "https") {
        return Ok(());
    }
    let rest = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
        .ok_or("not http or https")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if host.starts_with('[') {
        host.split_once(']').map(|(h, _)| h).unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    if host.trim_start_matches('[').is_empty() {
        return Err("no host");
    }
    Ok(())
}

/// Per-connection rate limit.
#[derive(Default)]
pub struct RateLimit(VecDeque<Instant>);

impl RateLimit {
    pub fn allow(&mut self, now: Instant) -> bool {
        while self
            .0
            .front()
            .is_some_and(|t| now.duration_since(*t) > RATE_WINDOW)
        {
            self.0.pop_front();
        }
        if self.0.len() >= RATE {
            return false;
        }
        self.0.push_back(now);
        true
    }
}

/// Opens a validated URL. Errors are reply statuses.
pub fn open(dbus: Option<&zbus::blocking::Connection>, url: &str, who: &str) -> Result<(), i32> {
    open_with_activation(dbus, url, who, "")
}

pub fn open_with_activation(
    dbus: Option<&zbus::blocking::Connection>,
    url: &str,
    who: &str,
    activation: &str,
) -> Result<(), i32> {
    if let Err(why) = validate(url) {
        crate::warn!(
            "openuri: {who}: refused ({why}): {:?}",
            crate::ipc::wire::truncate(url, 200)
        );
        return Err(-EINVAL);
    }
    crate::info!("openuri: {who}: {}", crate::ipc::wire::truncate(url, 512));
    let conn = dbus.ok_or(-ENOSYS)?;
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    if !activation.is_empty() {
        opts.insert("activation_token", Value::from(activation));
    }
    let r: zbus::Result<zbus::zvariant::OwnedObjectPath> = conn
        .call_method(
            Some("org.freedesktop.portal.Desktop"),
            "/org/freedesktop/portal/desktop",
            Some("org.freedesktop.portal.OpenURI"),
            "OpenURI",
            &("", url, opts),
        )
        .and_then(|m| m.body().deserialize());
    r.map(|_| ()).map_err(|e| {
        crate::warn!("openuri: portal: {e}");
        -EIO
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in [
            "mailto:",
            "mailto:a@b",
            "MAILTO:a@b?subject=Hello%20world",
            "tel:+123456",
            "https://example.com",
            "http://example.com:8080/a/b?c=d&e=%20#frag",
            "https://user@host/",
            "https://[::1]:443/x",
            "HTTPS://EXAMPLE.COM/",
        ] {
            assert_eq!(validate(ok), Ok(()), "{ok}");
        }
        for bad in [
            "",
            "file:///etc/passwd",
            "content://provider/file",
            "javascript:alert(1)",
            "https://",
            "https:///path",
            "https://exa mple.com",
            "https://example.com/\"onload",
            "https://example.com/\n",
            "https://ex\\ample.com",
            "-https://example.com",
            "https://é.com",
            "intent://x#Intent;end",
        ] {
            assert!(validate(bad).is_err(), "{bad}");
        }
        assert!(validate(&format!("https://a.b/{}", "x".repeat(MAX_URI))).is_err());
    }

    #[test]
    fn rate_limit() {
        let mut r = RateLimit::default();
        let t = Instant::now();
        for _ in 0..RATE {
            assert!(r.allow(t));
        }
        assert!(!r.allow(t));
        assert!(r.allow(t + RATE_WINDOW + Duration::from_millis(1)));
    }
}
