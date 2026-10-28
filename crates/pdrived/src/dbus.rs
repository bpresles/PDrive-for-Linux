// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! D-Bus service on the session bus, run on its own thread with a GLib main loop.
//!
//! - `io.github.bpresles.PDrive1` at `/io/github/bpresles/PDrive`: per-file state and offline
//!   actions, used by the Nautilus extension (`extension/`) and by the status window of
//!   `pdrive-app`: properties `State` (`syncing`, `idle`, `unmounted`, `error`), `Account`
//!   (address), `UsedSpace` and `MaxSpace` (bytes) with `PropertiesChanged`, `GetRecentActivity`
//!   (`a(sstxs)`: path, kind, size, timestamp, state), `Mount` / `Unmount`, signal `ActivityChanged`.
//! - The CloudProviders protocol at `/io/github/bpresles/PDrive/CloudProviders`, which makes the
//!   account appear in the sidebar of Nautilus and of the GTK file chooser, with its sync state
//!   and a menu. libcloudproviders finds it through `cloud-providers/io.github.bpresles.PDrive.ini`
//!   in a system data directory (see `scripts/install-system.sh`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gio::prelude::*;
use glib::variant::{ObjectPath, ToVariant, Variant};
use pdi18n::{tr, tr_fmt, tr_n};

use crate::drive::Drive;

pub const BUS_NAME: &str = "io.github.bpresles.PDrive";
const OBJECT_PATH: &str = "/io/github/bpresles/PDrive";
const INTERFACE: &str = "io.github.bpresles.PDrive1";

const CLOUD_PROVIDERS_PATH: &str = "/io/github/bpresles/PDrive/CloudProviders";
const PROVIDER_PATH: &str = "/io/github/bpresles/PDrive/CloudProviders/Provider";
const ACCOUNT_PATH: &str = "/io/github/bpresles/PDrive/CloudProviders/Account";
const PROVIDER_INTERFACE: &str = "org.freedesktop.CloudProviders.Provider";
const ACCOUNT_INTERFACE: &str = "org.freedesktop.CloudProviders.Account";
const OBJECT_MANAGER_INTERFACE: &str = "org.freedesktop.DBus.ObjectManager";

/// Name of the client (provider) and of the account shown in the sidebar.
const PROVIDER_NAME: &str = "PDrive";
const ACCOUNT_NAME: &str = "Proton Drive";
/// The application of `pdrive-app` (a `GApplication`).
const APP_BUS_NAME: &str = "io.github.bpresles.PDrive.App";
const APP_OBJECT_PATH: &str = "/io/github/bpresles/PDrive/App";
const APP_BINARY: &str = "pdrive-app";
const WEB_URL: &str = "https://drive.proton.me";
const ACTIVITY_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// CloudProvidersAccountStatus.
const STATUS_IDLE: i32 = 1;
const STATUS_SYNCING: i32 = 2;

const INTROSPECTION: &str = r#"
<node>
  <interface name="io.github.bpresles.PDrive1">
    <method name="GetFileStatus">
      <arg name="path" type="s" direction="in"/>
      <arg name="status" type="s" direction="out"/>
    </method>
    <method name="GetRecentActivity">
      <arg name="entries" type="a(sstxs)" direction="out"/>
    </method>
    <method name="Mount"/>
    <method name="Unmount"/>
    <method name="KeepOffline">
      <arg name="paths" type="as" direction="in"/>
    </method>
    <method name="FreeUpSpace">
      <arg name="paths" type="as" direction="in"/>
    </method>
    <property name="MountPoint" type="s" access="read"/>
    <property name="MountTarget" type="s" access="read"/>
    <property name="State" type="s" access="read"/>
    <property name="Account" type="s" access="read"/>
    <property name="UsedSpace" type="t" access="read"/>
    <property name="MaxSpace" type="t" access="read"/>
    <signal name="FileStatusChanged">
      <arg name="path" type="s"/>
    </signal>
    <signal name="ActivityChanged"/>
    <signal name="MountStateChanged">
      <arg name="mounted" type="b"/>
    </signal>
  </interface>
  <interface name="org.freedesktop.DBus.ObjectManager">
    <method name="GetManagedObjects">
      <arg name="objects" type="a{oa{sa{sv}}}" direction="out"/>
    </method>
    <signal name="InterfacesAdded">
      <arg name="object" type="o"/>
      <arg name="interfaces" type="a{sa{sv}}"/>
    </signal>
    <signal name="InterfacesRemoved">
      <arg name="object" type="o"/>
      <arg name="interfaces" type="as"/>
    </signal>
  </interface>
  <interface name="org.freedesktop.CloudProviders.Provider">
    <property name="Name" type="s" access="read"/>
  </interface>
  <interface name="org.freedesktop.CloudProviders.Account">
    <property name="Name" type="s" access="read"/>
    <property name="Path" type="s" access="read"/>
    <property name="Icon" type="s" access="read"/>
    <property name="Status" type="i" access="read"/>
    <property name="StatusDetails" type="s" access="read"/>
  </interface>
</node>
"#;

pub fn emit_file_status_changed(connection: &gio::DBusConnection, path: &Path) {
    let parameters = (path.to_string_lossy().as_ref(),).to_variant();
    let _ = connection.emit_signal(None, OBJECT_PATH, INTERFACE, "FileStatusChanged", Some(&parameters));
}

pub fn emit_mount_state_changed(connection: &gio::DBusConnection, mounted: bool) {
    let _ = connection.emit_signal(None, OBJECT_PATH, INTERFACE, "MountStateChanged", Some(&(mounted,).to_variant()));
}

pub fn emit_activity_changed(connection: &gio::DBusConnection) {
    let _ = connection.emit_signal(None, OBJECT_PATH, INTERFACE, "ActivityChanged", None);
}

/// Starts the service on a dedicated thread. Failures are logged: the mount works without it.
pub fn start(drive: Arc<Drive>, runtime: tokio::runtime::Handle) {
    let spawned = std::thread::Builder::new().name("dbus".into()).spawn(move || {
        // The default context: `glib::timeout_add_local` and friends attach to it, and nothing
        // else in the daemon uses it.
        let context = glib::MainContext::default();
        let main_loop = glib::MainLoop::new(Some(&context), false);
        let result = context.with_thread_default(|| {
            if let Err(e) = serve(&drive, &runtime) {
                eprintln!("D-Bus service unavailable: {e}");
                return;
            }
            main_loop.run();
        });
        if result.is_err() {
            eprintln!("D-Bus service unavailable: cannot acquire its main context");
        }
    });
    if let Err(e) = spawned {
        eprintln!("D-Bus service unavailable: {e}");
    }
}

fn serve(drive: &Arc<Drive>, runtime: &tokio::runtime::Handle) -> Result<(), glib::Error> {
    let connection = gio::bus_get_sync(gio::BusType::Session, None::<&gio::Cancellable>)?;
    let node = gio::DBusNodeInfo::for_xml(INTROSPECTION)?;
    let interface = |name: &str| node.lookup_interface(name).expect("interface in the introspection data");

    register_drive_interface(&connection, &interface(INTERFACE), drive, runtime)?;
    register_cloud_providers(&connection, &interface, drive)?;
    watch_properties(&connection, drive);

    gio::bus_own_name_on_connection(
        &connection,
        BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        |_, _| {},
        |_, name| eprintln!("D-Bus name {name} lost: is another instance running?"),
    );
    let _ = drive.activity_log.dbus.set(connection.clone());
    let _ = drive.dbus.set(connection);
    Ok(())
}

fn register_drive_interface(
    connection: &gio::DBusConnection,
    interface: &gio::DBusInterfaceInfo,
    drive: &Arc<Drive>,
    runtime: &tokio::runtime::Handle,
) -> Result<(), glib::Error> {
    let (method_drive, property_drive, runtime) = (drive.clone(), drive.clone(), runtime.clone());
    connection
        .register_object(OBJECT_PATH, interface)
        .method_call(move |_, _, _, _, method, parameters, invocation| match method {
            "GetFileStatus" => {
                let Some((path,)) = parameters.get::<(String,)>() else {
                    return invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "expected a path");
                };
                let status = method_drive.status_of_path(Path::new(&path));
                invocation.return_value(Some(&(status.as_str(),).to_variant()));
            }
            "GetRecentActivity" => {
                let entries: Vec<(String, String, u64, i64, String)> = method_drive
                    .activity_log
                    .recent()
                    .into_iter()
                    .map(|e| (e.path, e.kind.as_str().to_owned(), e.size, e.timestamp, e.state.as_str().to_owned()))
                    .collect();
                invocation.return_value(Some(&(entries,).to_variant()));
            }
            "Mount" | "Unmount" => {
                if method == "Mount" {
                    method_drive.request_mount();
                } else {
                    method_drive.request_unmount();
                }
                invocation.return_value(None);
            }
            "KeepOffline" | "FreeUpSpace" => {
                let Some((paths,)) = parameters.get::<(Vec<String>,)>() else {
                    return invocation.return_dbus_error("org.freedesktop.DBus.Error.InvalidArgs", "expected paths");
                };
                let keep = method == "KeepOffline";
                for ino in paths.iter().filter_map(|p| method_drive.ino_of_path(Path::new(p))) {
                    let drive = method_drive.clone();
                    runtime.spawn(async move {
                        if keep {
                            drive.keep_offline(ino).await;
                        } else {
                            drive.free_up_space(ino).await;
                        }
                    });
                }
                invocation.return_value(None);
            }
            _ => invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method),
        })
        .property(move |_, _, _, _, name| match name {
            "MountPoint" => property_drive.mountpoint.to_string_lossy().to_variant(),
            "MountTarget" => property_drive.mount_target.to_string_lossy().to_variant(),
            _ => drive_properties(&property_drive).remove(name).unwrap_or_else(|| "".to_variant()),
        })
        .build()?;
    Ok(())
}

/// The properties of the interface that change over time.
fn drive_properties(drive: &Drive) -> HashMap<&'static str, Variant> {
    let account = drive.account.lock().unwrap_or_else(|e| e.into_inner()).clone();
    HashMap::from([
        ("State", drive.state().to_variant()),
        ("Account", account.email.to_variant()),
        ("UsedSpace", account.used_space.to_variant()),
        ("MaxSpace", account.max_space.to_variant()),
    ])
}

/// Emits `PropertiesChanged` when `State`, `Account` or the quota change (checked by polling
/// in-memory state, like the sidebar status).
fn watch_properties(connection: &gio::DBusConnection, drive: &Arc<Drive>) {
    let (connection, drive) = (connection.clone(), drive.clone());
    let mut last = drive_properties(&drive);
    glib::timeout_add_local(ACTIVITY_POLL_INTERVAL, move || {
        let current = drive_properties(&drive);
        let changed: HashMap<&str, Variant> = current.iter().filter(|(name, value)| last.get(*name) != Some(value)).map(|(n, v)| (*n, v.clone())).collect();
        if !changed.is_empty() {
            let parameters = (INTERFACE, changed, Vec::<String>::new()).to_variant();
            let _ = connection.emit_signal(None, OBJECT_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&parameters));
            last = current;
        }
        glib::ControlFlow::Continue
    });
}

/// Sync state of the account for the sidebar; the tooltip tells it is a third-party client.
fn account_status(drive: &Drive) -> (i32, String) {
    let (status, details) = activity_status(drive);
    (status, tr_fmt("{} (unofficial client) · {}", &[PROVIDER_NAME, &details]))
}

fn activity_status(drive: &Drive) -> (i32, String) {
    if !drive.mounted.load(std::sync::atomic::Ordering::SeqCst) {
        return (STATUS_IDLE, tr("Not mounted: use Mount in the menu"));
    }
    let activity = drive.activity();
    let mut details = Vec::new();
    if activity.uploads > 0 {
        details.push(tr_n("{} file to upload", "{} files to upload", activity.uploads as u32, &[&activity.uploads.to_string()]));
    }
    if activity.downloads > 0 {
        details.push(tr_n("{} file downloading", "{} files downloading", activity.downloads as u32, &[&activity.downloads.to_string()]));
    }
    if details.is_empty() {
        (STATUS_IDLE, tr("Up to date"))
    } else {
        (STATUS_SYNCING, details.join(", "))
    }
}

fn account_properties(drive: &Drive) -> HashMap<String, Variant> {
    let (status, details) = account_status(drive);
    HashMap::from([
        ("Name".to_owned(), ACCOUNT_NAME.to_variant()),
        ("Path".to_owned(), drive.mountpoint.to_string_lossy().to_variant()),
        ("Icon".to_owned(), "folder-remote-symbolic".to_variant()),
        ("Status".to_owned(), status.to_variant()),
        ("StatusDetails".to_owned(), details.to_variant()),
    ])
}

fn register_cloud_providers(
    connection: &gio::DBusConnection,
    interface: &dyn Fn(&str) -> gio::DBusInterfaceInfo,
    drive: &Arc<Drive>,
) -> Result<(), glib::Error> {
    // Object manager listing the provider and its single account.
    let objects_drive = drive.clone();
    connection
        .register_object(CLOUD_PROVIDERS_PATH, &interface(OBJECT_MANAGER_INTERFACE))
        .method_call(move |_, _, _, _, method, _, invocation| {
            if method != "GetManagedObjects" {
                return invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method);
            }
            let provider = HashMap::from([("Name".to_owned(), PROVIDER_NAME.to_variant())]);
            let objects: HashMap<ObjectPath, HashMap<String, HashMap<String, Variant>>> = HashMap::from([
                (object_path(PROVIDER_PATH), HashMap::from([(PROVIDER_INTERFACE.to_owned(), provider)])),
                (object_path(ACCOUNT_PATH), HashMap::from([(ACCOUNT_INTERFACE.to_owned(), account_properties(&objects_drive))])),
            ]);
            invocation.return_value(Some(&(objects,).to_variant()));
        })
        .build()?;

    connection
        .register_object(PROVIDER_PATH, &interface(PROVIDER_INTERFACE))
        .property(|_, _, _, _, _| PROVIDER_NAME.to_variant())
        .build()?;

    let account_drive = drive.clone();
    connection
        .register_object(ACCOUNT_PATH, &interface(ACCOUNT_INTERFACE))
        .property(move |_, _, _, _, name| account_properties(&account_drive).remove(name).unwrap_or_else(|| "".to_variant()))
        .build()?;

    // Menu of the account in the sidebar; Nautilus inserts the actions under "cloudprovider".
    let menu = gio::Menu::new();
    // The first item toggles between "Mount" and "Unmount". File managers do not reliably
    // follow the enabled state of remote actions, but they read the menu model each time.
    let mut menu_mounted = drive.mounted.load(std::sync::atomic::Ordering::SeqCst);
    menu.append_item(&mount_menu_item(menu_mounted));
    menu.append(Some(&tr("Show status")), Some("cloudprovider.show-status"));
    menu.append(Some(&tr("Open in the browser")), Some("cloudprovider.open-web"));
    let actions = gio::SimpleActionGroup::new();
    let mount = gio::SimpleAction::new("mount", None);
    let mount_drive = drive.clone();
    mount.connect_activate(move |_, _| mount_drive.request_mount());
    actions.add_action(&mount);
    let unmount = gio::SimpleAction::new("unmount", None);
    let unmount_drive = drive.clone();
    unmount.connect_activate(move |_, _| unmount_drive.request_unmount());
    actions.add_action(&unmount);
    let open_web = gio::SimpleAction::new("open-web", None);
    open_web.connect_activate(|_, _| {
        if let Err(e) = gio::AppInfo::launch_default_for_uri(WEB_URL, None::<&gio::AppLaunchContext>) {
            eprintln!("cannot open {WEB_URL}: {e}");
        }
    });
    actions.add_action(&open_web);
    let show_status = gio::SimpleAction::new("show-status", None);
    let status_connection = connection.clone();
    show_status.connect_activate(move |_, _| show_status_window(&status_connection));
    actions.add_action(&show_status);
    connection.export_menu_model(ACCOUNT_PATH, &menu)?;
    connection.export_action_group(ACCOUNT_PATH, &actions)?;

    // Push state changes to the sidebar.
    let (poll_connection, poll_drive) = (connection.clone(), drive.clone());
    let mut last = account_status(drive);
    glib::timeout_add_local(ACTIVITY_POLL_INTERVAL, move || {
        let mounted = poll_drive.mounted.load(std::sync::atomic::Ordering::SeqCst);
        if mounted != menu_mounted {
            menu.remove(0);
            menu.insert_item(0, &mount_menu_item(mounted));
            menu_mounted = mounted;
        }
        let current = account_status(&poll_drive);
        if current != last {
            let changed = HashMap::from([
                ("Status".to_owned(), current.0.to_variant()),
                ("StatusDetails".to_owned(), current.1.to_variant()),
            ]);
            let parameters = (ACCOUNT_INTERFACE, changed, Vec::<String>::new()).to_variant();
            let _ = poll_connection.emit_signal(None, ACCOUNT_PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged", Some(&parameters));
            last = current;
        }
        glib::ControlFlow::Continue
    });
    Ok(())
}

fn mount_menu_item(mounted: bool) -> gio::MenuItem {
    if mounted {
        gio::MenuItem::new(Some(&tr("Unmount")), Some("cloudprovider.unmount"))
    } else {
        gio::MenuItem::new(Some(&tr("Mount")), Some("cloudprovider.mount"))
    }
}

fn object_path(path: &str) -> ObjectPath {
    ObjectPath::try_from(path.to_owned()).expect("valid object path")
}

/// Activates `pdrive-app` (which shows its status window), or starts it if it is not running.
fn show_status_window(connection: &gio::DBusConnection) {
    let spawn = || {
        // Next to this executable (installed together), otherwise in the PATH.
        let sibling = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.join(APP_BINARY))).filter(|p| p.exists());
        if let Err(e) = std::process::Command::new(sibling.unwrap_or_else(|| APP_BINARY.into())).spawn() {
            eprintln!("cannot start {APP_BINARY}: {e}");
        }
    };
    connection.call(
        Some(APP_BUS_NAME),
        APP_OBJECT_PATH,
        "org.freedesktop.Application",
        "Activate",
        Some(&(HashMap::<String, Variant>::new(),).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        5000,
        None::<&gio::Cancellable>,
        move |result| {
            if result.is_err() {
                spawn();
            }
        },
    );
}
