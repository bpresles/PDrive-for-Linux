//! StatusNotifierItem tray icon (`ksni`).
//!
//! ksni runs on tokio (zbus), GTK on the GLib main loop: the icon lives on its own thread with
//! a current-thread tokio runtime. Menu clicks go to the GTK side as [`Command`]s through an
//! `async-channel`, state changes come back the same way as [`TrayState`]s.

use ksni::menu::{MenuItem, StandardItem};
use ksni::{Category, ToolTip, TrayMethods};

use crate::config::APP_ID;
use pdi18n::tr;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Icon {
    Synced,
    Syncing,
    Error,
    Offline,
}

impl Icon {
    fn name(self) -> &'static str {
        match self {
            Icon::Synced => "pdrive-synced",
            Icon::Syncing => "pdrive-syncing",
            Icon::Error => "pdrive-error",
            Icon::Offline => "pdrive-offline",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Command {
    OpenFolder,
    ShowWindow,
    ToggleMount,
    Settings,
    Quit,
}

#[derive(Clone, PartialEq, Debug)]
pub struct TrayState {
    pub icon: Icon,
    /// Whether the menu offers "Unmount" rather than "Mount".
    pub mounted: bool,
    pub tooltip: String,
}

struct Tray {
    state: TrayState,
    commands: async_channel::Sender<Command>,
}

impl Tray {
    fn item(&self, label: String, icon: &str, command: Command) -> MenuItem<Self> {
        StandardItem {
            label,
            icon_name: icon.into(),
            activate: Box::new(move |tray: &mut Self| {
                let _ = tray.commands.try_send(command);
            }),
            ..Default::default()
        }
        .into()
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "PDrive".into()
    }

    fn category(&self) -> Category {
        Category::ApplicationStatus
    }

    fn icon_name(&self) -> String {
        self.state.icon.name().into()
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: "PDrive".into(), description: self.state.tooltip.clone(), ..Default::default() }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.commands.try_send(Command::ShowWindow);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mount = if self.state.mounted { tr("Unmount") } else { tr("Mount") };
        vec![
            self.item(tr("Open folder"), "folder-open-symbolic", Command::OpenFolder),
            self.item(tr("Show status window"), "view-list-symbolic", Command::ShowWindow),
            MenuItem::Separator,
            self.item(mount, "drive-harddisk-symbolic", Command::ToggleMount),
            self.item(tr("Settings"), "preferences-system-symbolic", Command::Settings),
            MenuItem::Separator,
            self.item(tr("Quit"), "application-exit-symbolic", Command::Quit),
        ]
    }

    fn watcher_online(&self) {
        eprintln!("tray: notification area available");
    }

    fn watcher_offline(&self, reason: ksni::OfflineReason) -> bool {
        eprintln!("tray: notification area gone ({reason:?}); waiting for it to come back");
        true
    }
}

/// Starts the icon on its own thread; returns the sender of state changes.
pub fn spawn(initial: TrayState, commands: async_channel::Sender<Command>) -> async_channel::Sender<TrayState> {
    let (sender, updates) = async_channel::unbounded::<TrayState>();
    let started = std::thread::Builder::new().name("tray".into()).spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(e) => return eprintln!("tray: cannot start the runtime: {e}"),
        };
        runtime.block_on(async move {
            // Without a watcher yet, ksni waits for one (`watcher_offline`) instead of failing.
            let handle = match (Tray { state: initial, commands }).assume_sni_available(true).spawn().await {
                Ok(handle) => handle,
                Err(e) => return eprintln!("tray: icon unavailable: {e}"),
            };
            while let Ok(state) = updates.recv().await {
                handle.update(|tray: &mut Tray| tray.state = state).await;
            }
        });
    });
    if let Err(e) = started {
        eprintln!("tray: cannot start the thread: {e}");
    }
    sender
}
