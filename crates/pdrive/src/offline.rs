// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! `pdrive keep-offline PATH...` and `pdrive free-up-space PATH...`: the same actions as the
//! Nautilus menu, through the D-Bus API of the daemon (`io.github.bpresles.PDrive1`). Meant for
//! file managers that can only run a command line (COSMIC Files) and for scripts.
//!
//! Such menus can be shown on every file, so paths outside the drive are ignored; when none is
//! inside it, a desktop notification says so and the exit status is non-zero.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use glib::variant::{ToVariant, Variant};
use pdi18n::tr;

const BUS_NAME: &str = "io.github.bpresles.PDrive";
const OBJECT_PATH: &str = "/io/github/bpresles/PDrive";
const INTERFACE: &str = "io.github.bpresles.PDrive1";
const APP_ICON: &str = "io.github.bpresles.PDrive.App";
const CALL_TIMEOUT_MS: i32 = 10_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    KeepOffline,
    FreeUpSpace,
}

impl Action {
    fn method(self) -> &'static str {
        match self {
            Action::KeepOffline => "KeepOffline",
            Action::FreeUpSpace => "FreeUpSpace",
        }
    }

    fn command(self) -> &'static str {
        match self {
            Action::KeepOffline => "keep-offline",
            Action::FreeUpSpace => "free-up-space",
        }
    }
}

/// Runs the action and returns the process exit status.
pub fn run(action: Action, args: &[String]) -> i32 {
    if args.is_empty() {
        eprintln!("usage: pdrive {} PATH...", action.command());
        return 2;
    }
    let connection = match gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>) {
        Ok(connection) => connection,
        Err(e) => return fail(None, &format!("{}: {e}", tr("Cannot reach the session bus"))),
    };
    let (mount_point, mount_target) = match (property(&connection, "MountPoint"), property(&connection, "MountTarget")) {
        (Ok(point), Ok(target)) => (point, target),
        (Err(e), _) | (_, Err(e)) => {
            let message = if is_daemon_missing(&e) { tr("PDrive is not running. Start it first.") } else { e.message().to_owned() };
            return fail(Some(&connection), &message);
        }
    };
    let prefixes = [PathBuf::from(mount_point), PathBuf::from(mount_target)];

    let (inside, ignored) = partition(args, &prefixes);
    for path in &ignored {
        eprintln!("{}: {}", tr("Ignored, not in Proton Drive"), path.display());
    }
    if inside.is_empty() {
        return fail(Some(&connection), &tr("These files are not in Proton Drive"));
    }
    let paths: Vec<String> = inside.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let call = connection.call_sync(
        Some(BUS_NAME),
        OBJECT_PATH,
        INTERFACE,
        action.method(),
        Some(&(paths,).to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    );
    match call {
        Ok(_) => 0,
        Err(e) => fail(Some(&connection), e.message()),
    }
}

fn is_daemon_missing(e: &glib::Error) -> bool {
    e.matches(gio::DBusError::ServiceUnknown) || e.matches(gio::DBusError::NameHasNoOwner)
}

fn property(connection: &gio::DBusConnection, name: &str) -> Result<String, glib::Error> {
    let reply = connection.call_sync(
        Some(BUS_NAME),
        OBJECT_PATH,
        "org.freedesktop.DBus.Properties",
        "Get",
        Some(&(INTERFACE, name).to_variant()),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    )?;
    let value = reply.try_child_value(0).and_then(|v| v.as_variant());
    Ok(value.and_then(|v| v.get::<String>()).unwrap_or_default())
}

/// Prints the message, shows it as a notification when a bus is available, and returns 1.
fn fail(connection: Option<&gio::DBusConnection>, message: &str) -> i32 {
    eprintln!("pdrive: {message}");
    if let Some(connection) = connection {
        notify(connection, message);
    }
    1
}

/// `org.freedesktop.Notifications.Notify`; `gio::Notification` needs an application. A missing
/// notification service is not an error.
fn notify(connection: &gio::DBusConnection, body: &str) {
    let hints: HashMap<String, Variant> = HashMap::new();
    let parameters = ("PDrive", 0u32, APP_ICON, "PDrive", body, Vec::<String>::new(), hints, -1i32).to_variant();
    let _ = connection.call_sync(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
        "Notify",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT_MS,
        None::<&gio::Cancellable>,
    );
}

/// Splits the arguments into the absolute paths inside one of the prefixes (as given, then with
/// symbolic links resolved) and the ones outside.
fn partition(args: &[String], prefixes: &[PathBuf]) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let mut inside = Vec::new();
    let mut ignored = Vec::new();
    for arg in args {
        let Ok(absolute) = std::path::absolute(arg) else {
            ignored.push(PathBuf::from(arg));
            continue;
        };
        let resolved = std::fs::canonicalize(&absolute).ok();
        match [Some(absolute.clone()), resolved].into_iter().flatten().find(|p| is_in(p, prefixes)) {
            Some(path) => inside.push(path),
            None => ignored.push(absolute),
        }
    }
    (inside, ignored)
}

fn is_in(path: &Path, prefixes: &[PathBuf]) -> bool {
    prefixes.iter().any(|prefix| !prefix.as_os_str().is_empty() && path.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn splits_paths_by_mount_prefix() {
        let prefixes = [PathBuf::from("/home/u/ProtonDrive"), PathBuf::from("/run/user/1000/pdrive")];
        let (inside, ignored) = partition(
            &strings(&["/home/u/ProtonDrive/a.txt", "/home/u/ProtonDrive", "/home/u/ProtonDriveX/b", "/tmp/c", "/run/user/1000/pdrive/d/e"]),
            &prefixes,
        );
        assert_eq!(inside, ["/home/u/ProtonDrive/a.txt", "/home/u/ProtonDrive", "/run/user/1000/pdrive/d/e"].map(PathBuf::from));
        assert_eq!(ignored, ["/home/u/ProtonDriveX/b", "/tmp/c"].map(PathBuf::from));
    }

    #[test]
    fn empty_prefixes_match_nothing() {
        let (inside, ignored) = partition(&strings(&["/a"]), &[PathBuf::new()]);
        assert!(inside.is_empty());
        assert_eq!(ignored, [PathBuf::from("/a")]);
    }

    #[test]
    fn relative_paths_are_made_absolute() {
        let cwd = std::env::current_dir().unwrap();
        let (inside, _) = partition(&strings(&["x.txt"]), std::slice::from_ref(&cwd));
        assert_eq!(inside, [cwd.join("x.txt")]);
    }

    #[test]
    fn symbolic_links_into_the_drive_count() {
        let dir = std::env::temp_dir().join(format!("pdrive-offline-test-{}", std::process::id()));
        let real = dir.join("drive");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::write(real.join("f"), "x").unwrap();
        std::os::unix::fs::symlink(&real, dir.join("link")).unwrap();
        let real = std::fs::canonicalize(&real).unwrap();
        let (inside, ignored) = partition(&[dir.join("link/f").to_string_lossy().into_owned()], std::slice::from_ref(&real));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(ignored.is_empty());
        assert_eq!(inside, [real.join("f")]);
    }
}
