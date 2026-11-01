//! Profiles ([`innkeeper_profile::profile`]) as the CLI and the manager show
//! them. Each profile runs Android or OpenHarmony. This module lists profiles
//! with their state and settings, and creates, renames, deletes, starts and
//! stops them. Each function works on the profile it is given, regardless of
//! this process's own profile.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use innkeeper_profile::profile;
use innkeeper_profile::profile::{FIRST, ProfileName, Settings};

use crate::launcher::Session;
use crate::paths::Prefix;
use crate::xdg::{self, Dirs};

/// A profile and its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    pub name: String,
    pub running: bool,
    pub settings: Settings,
}

fn paths(name: &str) -> Result<innkeeper_profile::Paths> {
    Prefix::locate()?.profile_paths(name)
}

fn names() -> Vec<ProfileName> {
    let mut v = profile::list(&innkeeper_profile::paths::data_home());
    // A test profile keeps its data in `INNKEEPER_DATA` and is named by
    // `INNKEEPER_PROFILE`. List it too.
    if let Ok(p) = innkeeper_profile::paths::profile()
        && std::env::var_os("INNKEEPER_DATA").is_some()
        && !v.contains(&p)
    {
        v.push(p);
    }
    v
}

/// Lists every profile. A profile whose settings can't be read shows the
/// defaults, and [`settings`] reports the error.
pub fn list() -> Result<Vec<Profile>> {
    names()
        .into_iter()
        .map(|name| {
            let p = paths(&name)?;
            Ok(Profile {
                running: innkeeper_profile::status(&p).pid.is_some(),
                settings: Settings::load(&p.data).unwrap_or_default(),
                name: name.into(),
            })
        })
        .collect()
}

/// Returns profile `name`, which must exist. [`FIRST`] is created if it is
/// missing, so the first command works with no setup.
pub fn existing(name: &str) -> Result<ProfileName> {
    let p = ProfileName::new(name)?;
    if !paths(&p)?.data.exists() {
        if name != FIRST {
            bail!("no profile {name} (`innkeeper profile create {name}` makes it)");
        }
        create(FIRST, &Settings::default())?;
    }
    Ok(p)
}

/// Profile `name`'s settings.
pub fn settings(name: &str) -> Result<Settings> {
    Settings::load(&paths(name)?.data)
}

/// Fails unless profile `name` runs Android. `what` names the Android-only
/// feature in the error.
pub fn need_android(name: &str, what: &str) -> Result<()> {
    settings(name)?
        .system
        .need_android(what)
        .with_context(|| format!("profile {name}"))
}

/// Loads the settings of this process's profile, named by
/// `INNKEEPER_PROFILE`. The bridge reads its settings this way.
pub fn own() -> Result<Settings> {
    Settings::load(&Prefix::locate()?.session_paths()?.data)
}

/// Changes profile `name`'s settings with `change` and saves them. A running
/// profile's bridge applies the live settings at once. Display settings take
/// effect at the next start.
pub fn update(name: &str, change: impl FnOnce(&mut Settings)) -> Result<Settings> {
    let data = paths(name)?.data;
    let mut s = Settings::load(&data)?;
    change(&mut s);
    s.save(&data)?;
    reload(name)?;
    Ok(s)
}

/// Asks profile `name`'s bridge, if it is running, to apply the saved
/// settings.
fn reload(name: &str) -> Result<()> {
    let connection = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(5))
        .build()?;
    let bus_name = crate::app_settings::bus_name(&ProfileName::new(name)?);
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    if !bus.name_has_owner(bus_name.as_str().try_into()?)? {
        return Ok(());
    }
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        bus_name.as_str(),
        crate::app_settings::PATH,
        crate::app_settings::INTERFACE,
    )?;
    let _: () = proxy
        .call("ReloadSettings", &())
        .context("saved, but the running profile did not apply it")?;
    Ok(())
}

/// Creates profile `name` with `settings`. The guest sets up its data at the
/// first start.
pub fn create(name: &str, settings: &Settings) -> Result<()> {
    let p = paths(name)?;
    if p.data.exists() {
        bail!("profile {name} exists already");
    }
    settings.save(&p.data)
}

/// Renames profile `from` to `to`, along with its apps' app-menu entries. The
/// profile must be stopped.
pub fn rename(from: &str, to: &str) -> Result<()> {
    profile::rename(&paths(from)?, &paths(to)?)?;
    xdg::move_entries(&Dirs::from_env(), from, to);
    Ok(())
}

/// Deletes profile `name` with its apps and their data, its settings and its
/// app-menu entries. The profile must be stopped.
pub fn delete(name: &str) -> Result<()> {
    let p = paths(name)?;
    if !p.data.exists() {
        bail!("no profile {name}");
    }
    profile::delete(&p)?;
    let d = Dirs::from_env();
    for app in xdg::desktop_apps(&d) {
        if app.profile.as_deref() == Some(name) {
            xdg::remove_app(&d, &app.package);
        }
    }
    Ok(())
}

/// Starts profile `name` with windows and waits for it to boot.
pub fn start(name: &str, status: &dyn Fn(&str)) -> Result<()> {
    Session::of_profile(name)?.ensure(status)
}

/// Deletes profile `name`'s Android data but keeps its settings, so Android
/// starts over at the next start. The profile must be stopped.
pub fn reset(name: &str) -> Result<()> {
    Session::of_profile(name)?.reset()
}

/// Prints profile `name`'s Android kernel log, following it with `follow`.
pub fn log(name: &str, follow: bool) -> Result<()> {
    Session::of_profile(name)?.log(follow)
}

/// Stops profile `name`, if it runs.
pub fn stop(name: &str) -> Result<()> {
    Session::of_profile(name)?.stop()
}
