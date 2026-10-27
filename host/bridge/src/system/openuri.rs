//! Opening web links from Android in the host browser, through the OpenURI
//! portal (or `xdg-open` without one). Only absolute `http`/`https` URLs in
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

/// Checks that `u` is an absolute http(s) URL with a host, in printable
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
    if let Err(why) = validate(url) {
        crate::warn!(
            "openuri: {who}: refused ({why}): {:?}",
            crate::ipc::wire::truncate(url, 200)
        );
        return Err(-EINVAL);
    }
    crate::info!("openuri: {who}: {}", crate::ipc::wire::truncate(url, 512));
    if let Some(conn) = dbus {
        let opts: HashMap<&str, Value<'_>> = HashMap::new();
        let r: zbus::Result<zbus::zvariant::OwnedObjectPath> = conn
            .call_method(
                Some("org.freedesktop.portal.Desktop"),
                "/org/freedesktop/portal/desktop",
                Some("org.freedesktop.portal.OpenURI"),
                "OpenURI",
                &("", url, opts),
            )
            .and_then(|m| m.body().deserialize());
        match r {
            Ok(_) => return Ok(()),
            Err(e) => crate::warn!("openuri: portal: {e}; trying xdg-open"),
        }
    }
    // No shell: the URL is one argv entry, and it starts with "http", so it
    // can't be taken for an option.
    match std::process::Command::new("xdg-open")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
            Ok(())
        }
        Err(e) => {
            crate::warn!("openuri: xdg-open: {e}");
            Err(-EIO)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        for ok in [
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
