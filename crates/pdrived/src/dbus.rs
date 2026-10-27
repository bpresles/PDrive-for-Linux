//! D-Bus service on the session bus, run on its own thread with a GLib main loop.
//!
//! - `io.github.pdrive.PDrive1` at `/io/github/pdrive/PDrive`: per-file state and offline
//!   actions, used by the Nautilus extension (`extension/`).
//! - The CloudProviders protocol at `/io/github/pdrive/PDrive/CloudProviders`, which makes the
//!   account appear in the sidebar of Nautilus and of the GTK file chooser, with its sync state
//!   and a menu. libcloudproviders finds it through `cloud-providers/io.github.pdrive.PDrive.ini`
//!   in a system data directory (see `scripts/install-system.sh`).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use gio::prelude::*;
use glib::variant::{ObjectPath, ToVariant, Variant};

use crate::drive::Drive;

pub const BUS_NAME: &str = "io.github.pdrive.PDrive";
const OBJECT_PATH: &str = "/io/github/pdrive/PDrive";
const INTERFACE: &str = "io.github.pdrive.PDrive1";

const CLOUD_PROVIDERS_PATH: &str = "/io/github/pdrive/PDrive/CloudProviders";
const PROVIDER_PATH: &str = "/io/github/pdrive/PDrive/CloudProviders/Provider";
const ACCOUNT_PATH: &str = "/io/github/pdrive/PDrive/CloudProviders/Account";
const PROVIDER_INTERFACE: &str = "org.freedesktop.CloudProviders.Provider";
const ACCOUNT_INTERFACE: &str = "org.freedesktop.CloudProviders.Account";
const OBJECT_MANAGER_INTERFACE: &str = "org.freedesktop.DBus.ObjectManager";

/// Name of the client (provider) and of the account shown in the sidebar.
const PROVIDER_NAME: &str = "PDrive";
const ACCOUNT_NAME: &str = "Proton Drive";
const WEB_URL: &str = "https://drive.proton.me";
const ACTIVITY_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// CloudProvidersAccountStatus.
const STATUS_IDLE: i32 = 1;
const STATUS_SYNCING: i32 = 2;

const INTROSPECTION: &str = r#"
<node>
  <interface name="io.github.pdrive.PDrive1">
    <method name="GetFileStatus">
      <arg name="path" type="s" direction="in"/>
      <arg name="status" type="s" direction="out"/>
    </method>
    <method name="KeepOffline">
      <arg name="paths" type="as" direction="in"/>
    </method>
    <method name="FreeUpSpace">
      <arg name="paths" type="as" direction="in"/>
    </method>
    <property name="MountPoint" type="s" access="read"/>
    <signal name="FileStatusChanged">
      <arg name="path" type="s"/>
    </signal>
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

    gio::bus_own_name_on_connection(
        &connection,
        BUS_NAME,
        gio::BusNameOwnerFlags::NONE,
        |_, _| {},
        |_, name| eprintln!("D-Bus name {name} lost: is another instance running?"),
    );
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
            _ => "".to_variant(),
        })
        .build()?;
    Ok(())
}

/// Sync state of the account for the sidebar; the tooltip tells it is a third-party client.
fn account_status(drive: &Drive) -> (i32, String) {
    let (status, details) = activity_status(drive);
    (status, format!("{PROVIDER_NAME} (unofficial client) · {details}"))
}

fn activity_status(drive: &Drive) -> (i32, String) {
    if !drive.mounted.load(std::sync::atomic::Ordering::SeqCst) {
        return (STATUS_IDLE, "Not mounted: use Mount in the menu".to_owned());
    }
    let activity = drive.activity();
    let mut details = Vec::new();
    if activity.uploads > 0 {
        details.push(format!("{} file(s) to upload", activity.uploads));
    }
    if activity.downloads > 0 {
        details.push(format!("{} file(s) downloading", activity.downloads));
    }
    if details.is_empty() {
        (STATUS_IDLE, "Up to date".to_owned())
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
    menu.append(Some("Open in the browser"), Some("cloudprovider.open-web"));
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
        gio::MenuItem::new(Some("Unmount"), Some("cloudprovider.unmount"))
    } else {
        gio::MenuItem::new(Some("Mount"), Some("cloudprovider.mount"))
    }
}

fn object_path(path: &str) -> ObjectPath {
    ObjectPath::try_from(path.to_owned()).expect("valid object path")
}
