// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! PDrive for Linux desktop application.
//!
//!     pdrive-app [--background]
//!     pdrive-app --remove-integrations
//!
//! Single instance (`io.github.bpresles.PDrive.App`) that stays alive without a window:
//! it runs and supervises `pdrived`, shows the tray icon (StatusNotifierItem) and the status
//! window, and offers the settings. Starting it again shows the status window; with
//! `--background` (autostart) no window is opened. Only "Quit" in the tray menu (or SIGTERM)
//! ends it, after stopping the daemon. The logs of the daemon go to this program's standard
//! error output, which `logs.rs` sends to the journal when it would be lost.
//!
//! `--remove-integrations` removes the entries added to the file managers (`integrations.rs`)
//! and exits; the uninstall script runs it.

mod about;
mod config;
mod daemon;
mod integrations;
mod logs;
mod settings;
mod supervisor;
mod tray;
mod update;
mod window;

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use config::APP_ID;
use daemon::Daemon;
use pdi18n::{tr, tr_fmt};
use supervisor::{Supervisor, program};
use tray::{Command, Icon, TrayState};
use window::{Status, StatusWindow, View};

const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
/// With `--background`, how long to wait for the notification area before opening the window.
const WATCHER_GRACE: Duration = Duration::from_secs(5);

struct App {
    app: adw::Application,
    supervisor: Rc<Supervisor>,
    daemon: Rc<Daemon>,
    window: RefCell<Option<Rc<StatusWindow>>>,
    tray: async_channel::Sender<TrayState>,
    last_tray: RefCell<Option<TrayState>>,
    /// Newer release announced by the update check.
    update: RefCell<Option<String>>,
    checker: update::Checker,
    watcher_present: Cell<bool>,
    background: Cell<bool>,
    /// A command line was handled already.
    launched: Cell<bool>,
    /// `pdrive login` is running.
    signing_in: Cell<bool>,
    activity_pending: Cell<bool>,
    quitting: Cell<bool>,
}

/// Blocks SIGTERM and SIGINT in every thread (called before any other thread exists) and turns
/// them into messages: the shutdown must go through the main loop, which stops the daemon.
fn terminate_signals() -> async_channel::Receiver<()> {
    let (sender, receiver) = async_channel::unbounded();
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        let _ = std::thread::Builder::new().name("signals".into()).spawn(move || {
            loop {
                let mut signal = 0;
                if libc::sigwait(&set, &mut signal) == 0 && sender.try_send(()).is_err() {
                    break;
                }
            }
        });
    }
    receiver
}

fn main() -> glib::ExitCode {
    if std::env::args().any(|a| a == "--remove-integrations") {
        pdi18n::init();
        return if integrations::remove_all() { glib::ExitCode::SUCCESS } else { glib::ExitCode::FAILURE };
    }
    logs::init();
    let terminate = terminate_signals();
    pdi18n::init();
    config::migrate_legacy_autostart();
    let app = adw::Application::builder().application_id(APP_ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    let state: Rc<OnceCell<Rc<App>>> = Rc::new(OnceCell::new());

    app.connect_startup({
        let state = state.clone();
        move |app| {
            // Alive without a window until "Quit".
            std::mem::forget(app.hold());
            let _ = state.set(App::new(app, terminate.clone()));
        }
    });
    app.connect_command_line({
        let state = state.clone();
        move |_, command_line| {
            let background = command_line.arguments().iter().any(|a| a == "--background");
            if let Some(this) = state.get() {
                // The first invocation decides how the application started; later ones come from
                // the user starting it again, and show the window.
                let first = !this.launched.replace(true);
                if first {
                    this.background.set(background);
                }
                if !background && (!first || config::show_window_at_startup()) {
                    this.show_window();
                }
            }
            0.into()
        }
    });
    app.connect_activate({
        let state = state.clone();
        move |_| {
            if let Some(this) = state.get() {
                this.show_window();
            }
        }
    });
    app.run()
}

impl App {
    fn new(app: &adw::Application, terminate: async_channel::Receiver<()>) -> Rc<Self> {
        let (commands, command_receiver) = async_channel::unbounded();
        let tray = tray::spawn(TrayState { icon: Icon::Offline, mounted: false, tooltip: Status::Starting.label(), update: None }, commands);

        // The supervisor and the daemon client call back into `App`, which does not exist yet.
        let this: Rc<RefCell<Option<std::rc::Weak<App>>>> = Rc::new(RefCell::new(None));
        let refresh = {
            let this = this.clone();
            move || {
                if let Some(app) = this.borrow().as_ref().and_then(|w| w.upgrade()) {
                    app.refresh();
                }
            }
        };
        let supervisor = Supervisor::new(refresh.clone(), {
            let this = this.clone();
            move || {
                if let Some(app) = this.borrow().as_ref().and_then(|w| w.upgrade()) {
                    app.session_expired();
                }
            }
        });
        let daemon = Daemon::connect({
            let this = this.clone();
            move |activity| {
                if let Some(app) = this.borrow().as_ref().and_then(|w| w.upgrade()) {
                    app.refresh();
                    if activity {
                        app.refresh_activity();
                    }
                }
            }
        })
        .expect("session bus");

        let (checker, reports) = update::spawn();
        let me = Rc::new(App {
            app: app.clone(),
            supervisor,
            daemon,
            window: RefCell::new(None),
            tray,
            last_tray: RefCell::new(None),
            update: RefCell::new(None),
            checker,
            watcher_present: Cell::new(true),
            background: Cell::new(false),
            launched: Cell::new(false),
            signing_in: Cell::new(false),
            activity_pending: Cell::new(false),
            quitting: Cell::new(false),
        });
        *this.borrow_mut() = Some(Rc::downgrade(&me));

        me.add_actions();
        me.watch_names();
        me.supervisor.ensure_running();
        // Decided by what is installed, at every start: a file manager installed since is
        // taken into account.
        glib::spawn_future_local(async {
            if let Ok(errors) = gio::spawn_blocking(integrations::apply_from_settings).await {
                integrations::report(errors);
            }
        });

        let weak = Rc::downgrade(&me);
        glib::spawn_future_local(async move {
            while let Ok(report) = reports.recv().await {
                let Some(me) = weak.upgrade() else { break };
                me.handle_update_report(report);
            }
        });
        let weak = Rc::downgrade(&me);
        glib::spawn_future_local(async move {
            while let Ok(command) = command_receiver.recv().await {
                let Some(me) = weak.upgrade() else { break };
                match command {
                    Command::OpenFolder => me.open_folder(),
                    Command::ShowWindow => me.show_window(),
                    Command::ToggleMount => me.toggle_mount(),
                    Command::Settings => me.show_settings(),
                    Command::About => me.show_about(),
                    Command::CheckUpdate => me.checker.check_now(),
                    Command::OpenUpdate => window::open_uri(None, update::RELEASES_URL),
                    Command::Quit => me.quit(),
                }
            }
        });
        let weak = Rc::downgrade(&me);
        glib::spawn_future_local(async move {
            while terminate.recv().await.is_ok() {
                let Some(me) = weak.upgrade() else { break };
                me.quit();
            }
        });
        me
    }

    fn add_actions(self: &Rc<Self>) {
        let add = |name: &str, handler: fn(&Rc<App>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(me) = weak.upgrade() {
                    handler(&me);
                }
            });
            self.app.add_action(&action);
        };
        add("settings", App::show_settings);
        add("about", App::show_about);
        add("sign-in", App::sign_in);
        add("open-folder", App::open_folder);
        add("toggle-mount", App::toggle_mount);
    }

    /// Follows the daemon name (to start it when it vanishes) and the notification area.
    fn watch_names(self: &Rc<Self>) {
        let (daemon_events, daemon_receiver) = async_channel::unbounded::<()>();
        let vanished = daemon_events.clone();
        // The callbacks may run on another thread: they only post into a channel.
        gio::bus_watch_name(
            gio::BusType::Session,
            daemon::BUS_NAME,
            gio::BusNameWatcherFlags::NONE,
            |_, _, _| {},
            move |_, _| {
                let _ = vanished.try_send(());
            },
        );
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while daemon_receiver.recv().await.is_ok() {
                let Some(me) = weak.upgrade() else { break };
                me.supervisor.ensure_running();
            }
        });

        let (watcher_events, watcher_receiver) = async_channel::unbounded::<bool>();
        let appeared = watcher_events.clone();
        gio::bus_watch_name(
            gio::BusType::Session,
            WATCHER_NAME,
            gio::BusNameWatcherFlags::NONE,
            move |_, _, _| {
                let _ = appeared.try_send(true);
            },
            move |_, _| {
                let _ = watcher_events.try_send(false);
            },
        );
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut first = true;
            while let Ok(present) = watcher_receiver.recv().await {
                let Some(me) = weak.upgrade() else { break };
                me.watcher_present.set(present);
                if let Some(window) = me.window.borrow().as_ref() {
                    window.set_notification_area_missing(!present);
                }
                if !present && first {
                    // Nothing to click: show the window with the explanation (autostart gives
                    // the desktop a moment to start its notification area).
                    if me.background.get() {
                        let weak = weak.clone();
                        glib::timeout_add_local_once(WATCHER_GRACE, move || {
                            if let Some(me) = weak.upgrade().filter(|me| !me.watcher_present.get()) {
                                me.show_window();
                            }
                        });
                    } else {
                        me.show_window();
                    }
                }
                first = false;
            }
        });
    }

    fn window(self: &Rc<Self>) -> Rc<StatusWindow> {
        if let Some(window) = self.window.borrow().as_ref() {
            return window.clone();
        }
        let window = StatusWindow::new(&self.app);
        window.set_notification_area_missing(!self.watcher_present.get());
        *self.window.borrow_mut() = Some(window.clone());
        self.refresh();
        window
    }

    fn show_window(self: &Rc<Self>) {
        let window = self.window();
        window.window.present();
        self.refresh_activity();
    }

    fn status(&self) -> Status {
        let snapshot = self.daemon.snapshot();
        match self.supervisor.signed_in() {
            Some(false) => Status::SignedOut,
            None => Status::Starting,
            Some(true) if !snapshot.connected => {
                if self.supervisor.active() {
                    Status::Starting
                } else {
                    Status::Stopped
                }
            }
            Some(true) => match snapshot.state.as_str() {
                "syncing" => Status::Syncing,
                "unmounted" => Status::Unmounted,
                "error" => Status::Error,
                _ => Status::Idle,
            },
        }
    }

    /// Pushes the current state to the window and the tray icon.
    fn refresh(&self) {
        let snapshot = self.daemon.snapshot();
        let status = self.status();
        if let Some(window) = self.window.borrow().as_ref() {
            window.update(&View { status, account: snapshot.account.clone(), used_space: snapshot.used_space, max_space: snapshot.max_space });
        }
        let update = self.update.borrow().clone();
        let icon = match status {
            Status::Idle => Icon::Synced,
            Status::Syncing => Icon::Syncing,
            Status::Error => Icon::Error,
            _ => Icon::Offline,
        };
        let state = TrayState { icon, mounted: matches!(status, Status::Idle | Status::Syncing | Status::Error), tooltip: status.label(), update };
        if self.last_tray.borrow().as_ref() != Some(&state) {
            let _ = self.tray.try_send(state.clone());
            *self.last_tray.borrow_mut() = Some(state);
        }
    }

    /// Reloads the activity list (grouping bursts of changes).
    fn refresh_activity(self: &Rc<Self>) {
        let visible = self.window.borrow().as_ref().is_some_and(|w| w.window.is_visible());
        if !visible || self.activity_pending.replace(true) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::timeout_add_local_once(Duration::from_millis(300), move || {
            let Some(me) = weak.upgrade() else { return };
            me.activity_pending.set(false);
            glib::spawn_future_local(async move {
                let entries = me.daemon.recent_activity().await;
                if let Some(window) = me.window.borrow().as_ref() {
                    window.set_activity(entries);
                }
            });
        });
    }

    fn mountpoint(&self) -> std::path::PathBuf {
        let snapshot = self.daemon.snapshot();
        if snapshot.mountpoint.is_empty() { config::expand_home(&config::load().mountpoint) } else { snapshot.mountpoint.into() }
    }

    fn open_folder(self: &Rc<Self>) {
        let uri = gio::File::for_path(self.mountpoint()).uri();
        if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
            eprintln!("cannot open {uri}: {e}");
        }
    }

    fn toggle_mount(self: &Rc<Self>) {
        let mounted = matches!(self.status(), Status::Idle | Status::Syncing | Status::Error);
        self.daemon.set_mounted(!mounted);
    }

    fn show_about(self: &Rc<Self>) {
        let window = self.window();
        window.window.present();
        about::show(&window.window);
    }

    fn show_settings(self: &Rc<Self>) {
        let window = self.window();
        window.window.present();
        let (restart, sign_out, check) = (Rc::downgrade(self), Rc::downgrade(self), Rc::downgrade(self));
        settings::show(
            &window.window,
            &self.daemon.snapshot().account,
            self.update.borrow().clone(),
            Rc::new(move || {
                if let Some(me) = check.upgrade() {
                    me.checker.check_now();
                }
            }),
            Rc::new(move || {
                if let Some(me) = restart.upgrade() {
                    me.restart_daemon();
                }
            }),
            Rc::new(move || {
                if let Some(me) = sign_out.upgrade() {
                    me.sign_out();
                }
            }),
        );
    }

    fn restart_daemon(self: &Rc<Self>) {
        let me = self.clone();
        self.supervisor.stop(move || {
            me.supervisor.resume();
            me.supervisor.ensure_running();
        });
    }

    /// The daemon stopped because the session is refused (expired, revoked) or missing: signs in
    /// again through the browser at once, with the window explaining why the browser opens.
    fn session_expired(self: &Rc<Self>) {
        eprintln!("the session is no longer valid: signing in again through the browser");
        let window = self.window();
        window.set_session_expired(true);
        window.window.present();
        self.sign_in();
    }

    /// `pdrive login` (browser sign-in), then the daemon starts.
    fn sign_in(self: &Rc<Self>) {
        if self.signing_in.replace(true) {
            return;
        }
        let window = self.window();
        window.set_signing_in(true);
        let me = self.clone();
        glib::spawn_future_local(async move {
            let status = gio::spawn_blocking(|| std::process::Command::new(program("pdrive")).arg("login").status()).await;
            me.signing_in.set(false);
            window.set_signing_in(false);
            if matches!(status, Ok(Ok(status)) if status.success()) {
                window.set_session_expired(false);
                me.supervisor.set_signed_in(true);
                me.supervisor.resume();
                me.supervisor.ensure_running();
                me.offer_autostart();
            } else {
                me.alert(&tr("Sign-in failed"), &tr("The sign-in did not complete. Try again."));
            }
        });
    }

    /// Stops the daemon, then `pdrive logout`.
    fn sign_out(self: &Rc<Self>) {
        let me = self.clone();
        self.supervisor.stop(move || {
            glib::spawn_future_local(async move {
                let status = gio::spawn_blocking(|| std::process::Command::new(program("pdrive")).arg("logout").status()).await;
                if matches!(status, Ok(Ok(status)) if status.success()) {
                    me.supervisor.set_signed_in(false);
                } else {
                    me.alert(&tr("Sign-out failed"), &tr("The session could not be removed. See the logs of the application."));
                    me.supervisor.resume();
                    me.supervisor.ensure_running();
                }
            });
        });
    }

    /// Proposed once after the first sign-in.
    fn offer_autostart(self: &Rc<Self>) {
        if config::autostart_enabled() {
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some(&tr("Start PDrive when you log in?")),
            Some(&tr("The Proton Drive folder is then mounted automatically. You can change this in the settings.")),
        );
        dialog.add_responses(&[("no", &tr("Not now")), ("yes", &tr("Start at login"))]);
        dialog.set_response_appearance("yes", adw::ResponseAppearance::Suggested);
        dialog.set_close_response("no");
        dialog.connect_response(None, |_, response| {
            if response == "yes"
                && let Err(e) = config::set_autostart(true)
            {
                eprintln!("cannot enable the autostart: {e:#}");
            }
        });
        dialog.present(Some(&self.window().window));
    }

    /// Remembers the newest release (tray menu, settings) and answers the checks that need it.
    fn handle_update_report(self: &Rc<Self>, report: update::Report) {
        use update::{Kind, Outcome};
        let manual = report.kind == Kind::Manual;
        match report.outcome {
            Outcome::Newer(version) => {
                *self.update.borrow_mut() = Some(version.clone());
                self.refresh();
                if report.kind != Kind::Background {
                    self.offer_update(&version);
                }
            }
            Outcome::UpToDate => {
                *self.update.borrow_mut() = None;
                self.refresh();
                if manual {
                    self.notify(&tr("You already have the latest version"), "");
                }
            }
            Outcome::Failed(error) => {
                if manual {
                    self.notify(&tr("Cannot check for updates"), &error);
                }
            }
        }
    }

    /// "Ignore" or "Update" (opens the latest release page). Own window: the status window is not
    /// opened for it.
    fn offer_update(self: &Rc<Self>, version: &str) {
        let dialog = gtk::AlertDialog::builder()
            .message(tr_fmt("New version is available: v{}", &[version]))
            .buttons([tr("Ignore"), tr("Update")])
            .cancel_button(0)
            .default_button(1)
            .build();
        dialog.choose(gtk::Window::NONE, gtk::gio::Cancellable::NONE, |response| {
            if response == Ok(1) {
                window::open_uri(None, update::RELEASES_URL);
            }
        });
    }

    /// Result of a manual check that needs no action, in its own window (see `offer_update`).
    fn notify(self: &Rc<Self>, title: &str, body: &str) {
        let dialog = gtk::AlertDialog::builder().message(title).detail(body).buttons([tr("OK")]).build();
        dialog.choose(gtk::Window::NONE, gtk::gio::Cancellable::NONE, |_| {});
    }

    fn alert(self: &Rc<Self>, title: &str, body: &str) {
        let dialog = adw::AlertDialog::new(Some(title), (!body.is_empty()).then_some(body));
        dialog.add_response("ok", &tr("OK"));
        dialog.present(Some(&self.window().window));
    }

    /// Stops the daemon cleanly, then ends the application.
    fn quit(self: &Rc<Self>) {
        if self.quitting.replace(true) {
            return;
        }
        let app = self.app.clone();
        self.supervisor.stop(move || app.quit());
    }
}
