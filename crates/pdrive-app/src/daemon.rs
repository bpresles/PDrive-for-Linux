//! Client of the D-Bus interface of `pdrived` (`io.github.bpresles.PDrive1`).

use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

pub const BUS_NAME: &str = "io.github.bpresles.PDrive";
const OBJECT_PATH: &str = "/io/github/bpresles/PDrive";
const INTERFACE: &str = "io.github.bpresles.PDrive1";

#[derive(Clone, Default)]
pub struct Snapshot {
    /// The daemon owns its bus name and its properties are loaded.
    pub connected: bool,
    /// `syncing`, `idle`, `unmounted` or `error`.
    pub state: String,
    pub account: String,
    pub used_space: u64,
    pub max_space: u64,
    pub mountpoint: String,
}

#[derive(Clone, Debug)]
pub struct Activity {
    pub path: String,
    pub kind: String,
    pub size: u64,
    pub timestamp: i64,
    pub state: String,
}

pub struct Daemon {
    proxy: gio::DBusProxy,
}

impl Daemon {
    /// Follows the daemon, whether it is running yet or not; `on_change` is called when its
    /// properties change, when it appears or vanishes, and on `ActivityChanged`
    /// (the argument tells which).
    pub fn connect(on_change: impl Fn(bool) + 'static) -> Result<Rc<Self>, glib::Error> {
        let proxy = gio::DBusProxy::for_bus_sync(
            gio::BusType::Session,
            gio::DBusProxyFlags::DO_NOT_AUTO_START,
            None,
            BUS_NAME,
            OBJECT_PATH,
            INTERFACE,
            None::<&gio::Cancellable>,
        )?;
        let on_change = Rc::new(on_change);
        // `connect_local`: the callbacks are not `Send`, and they run on this thread.
        proxy.connect_local("g-properties-changed", false, {
            let on_change = on_change.clone();
            move |_| {
                on_change(false);
                None
            }
        });
        proxy.connect_local("g-signal", false, {
            let on_change = on_change.clone();
            move |values| {
                if values.get(2).and_then(|v| v.get::<String>().ok()).as_deref() == Some("ActivityChanged") {
                    on_change(true);
                }
                None
            }
        });
        proxy.connect_notify_local(Some("g-name-owner"), move |_, _| on_change(true));
        Ok(Rc::new(Self { proxy }))
    }

    pub fn snapshot(&self) -> Snapshot {
        let string = |name: &str| self.proxy.cached_property(name).and_then(|v| v.str().map(str::to_owned));
        let number = |name: &str| self.proxy.cached_property(name).and_then(|v| v.get::<u64>()).unwrap_or(0);
        let state = string("State");
        Snapshot {
            connected: self.proxy.name_owner().is_some() && state.is_some(),
            state: state.unwrap_or_default(),
            account: string("Account").unwrap_or_default(),
            used_space: number("UsedSpace"),
            max_space: number("MaxSpace"),
            mountpoint: string("MountPoint").unwrap_or_default(),
        }
    }

    pub async fn recent_activity(&self) -> Vec<Activity> {
        let reply = self.proxy.call_future("GetRecentActivity", None, gio::DBusCallFlags::NONE, 5000).await;
        let entries = reply.ok().and_then(|r| r.get::<(Vec<(String, String, u64, i64, String)>,)>()).map(|(e,)| e);
        entries
            .unwrap_or_default()
            .into_iter()
            .map(|(path, kind, size, timestamp, state)| Activity { path, kind, size, timestamp, state })
            .collect()
    }

    pub fn set_mounted(&self, mounted: bool) {
        let method = if mounted { "Mount" } else { "Unmount" };
        self.proxy.call(method, None, gio::DBusCallFlags::NONE, 5000, None::<&gio::Cancellable>, |result| {
            if let Err(e) = result {
                eprintln!("mount request failed: {e}");
            }
        });
    }
}
