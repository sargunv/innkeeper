//! Unity LauncherEntry, which Plasma, Ubuntu Dock and Dash to Dock
//! understand. Its state comes from Android notifications, including those
//! suppressed by Do Not Disturb.

use std::collections::{HashMap, HashSet};

use zbus::blocking::Connection;
use zbus::zvariant::Value;

use super::notify::{Note, flags};

#[derive(Default)]
pub struct LauncherEntry {
    notes: HashMap<(u64, u32), Entry>,
}

struct Entry {
    package: String,
    progress: i32,
    badge: bool,
    urgent: bool,
}

impl LauncherEntry {
    pub fn post(&mut self, conn: &Connection, client: u64, id: u32, n: &Note) {
        let old = self.notes.insert(
            (client, id),
            Entry {
                package: n.package.clone(),
                progress: n.progress,
                badge: n.flags & flags::BADGE != 0 && n.flags & flags::ONGOING == 0,
                urgent: n.urgency == 2,
            },
        );
        if let Some(old) = old.filter(|o| o.package != n.package) {
            self.publish(conn, &old.package);
        }
        self.publish(conn, &n.package);
    }

    pub fn close(&mut self, conn: &Connection, client: u64, id: u32) {
        if let Some(n) = self.notes.remove(&(client, id)) {
            self.publish(conn, &n.package);
        }
    }

    pub fn count(&self, client: u64) -> usize {
        self.notes.keys().filter(|(c, _)| *c == client).count()
    }

    pub fn contains(&self, client: u64, id: u32) -> bool {
        self.notes.contains_key(&(client, id))
    }

    pub fn ids(&self, client: u64) -> Vec<u32> {
        self.notes
            .keys()
            .filter(|(c, _)| *c == client)
            .map(|(_, id)| *id)
            .collect()
    }

    pub fn clear(&mut self, conn: &Connection) {
        let packages: HashSet<String> = self.notes.drain().map(|(_, n)| n.package).collect();
        for package in packages {
            self.publish(conn, &package);
        }
    }

    fn publish(&self, conn: &Connection, package: &str) {
        let mut count = 0i64;
        let mut sum = 0;
        let mut tasks = 0;
        let mut urgent = false;
        for n in self.notes.values().filter(|n| n.package == package) {
            count += i64::from(n.badge);
            urgent |= n.urgent;
            if n.progress != -1 {
                // Unity has no indeterminate state, so indeterminate progress
                // shows as 50%. Concurrent tasks are averaged with equal
                // weight.
                sum += if n.progress == -2 { 50 } else { n.progress };
                tasks += 1;
            }
        }
        let progress = if tasks == 0 {
            0.0
        } else {
            f64::from(sum) / f64::from(tasks) / 100.0
        };
        let properties: HashMap<&str, Value<'_>> = HashMap::from([
            ("progress", Value::from(progress)),
            ("progress-visible", Value::from(tasks != 0)),
            ("count", Value::from(count)),
            ("count-visible", Value::from(count != 0)),
            ("urgent", Value::from(urgent)),
        ]);
        let uri = format!(
            "application://{}.desktop",
            innkeeper_core::desktop::app_id(package)
        );
        if let Err(e) = conn.emit_signal(
            None::<&str>,
            "/com/canonical/Unity/LauncherEntry",
            "com.canonical.Unity.LauncherEntry",
            "Update",
            &(uri, properties),
        ) {
            crate::warn!("launcher entry: {package}: {e}");
        }
    }
}
