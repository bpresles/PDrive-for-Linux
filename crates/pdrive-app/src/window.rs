// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Status window: account and storage, recent activity, state bar.

use std::cell::RefCell;

use adw::prelude::*;
use gtk::glib;

use crate::config::APP_ID;
use crate::daemon::Activity;
use pdi18n::{tr, tr_fmt, tr_n};

pub const WEB_URL: &str = "https://drive.proton.me";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    SignedOut,
    Starting,
    /// The daemon is not running and not being started (waiting to retry).
    Stopped,
    Unmounted,
    Syncing,
    Idle,
    Error,
}

impl Status {
    pub fn label(self) -> String {
        match self {
            Status::SignedOut => tr("Not signed in"),
            Status::Starting => tr("Starting…"),
            Status::Stopped => tr("Service not running"),
            Status::Unmounted => tr("Not mounted"),
            Status::Syncing => tr("Syncing…"),
            Status::Idle => tr("Up to date"),
            Status::Error => tr("Error: an upload failed"),
        }
    }
}

pub struct View {
    pub status: Status,
    pub account: String,
    pub used_space: u64,
    pub max_space: u64,
}

pub struct StatusWindow {
    pub window: adw::ApplicationWindow,
    banner: adw::Banner,
    stack: gtk::Stack,
    sign_in: gtk::Button,
    avatar: adw::Avatar,
    account: gtk::Label,
    quota: gtk::Label,
    list: gtk::ListBox,
    status: gtk::Label,
    spinner: gtk::Spinner,
    mount: gtk::Button,
    activity: RefCell<Vec<Activity>>,
}

impl StatusWindow {
    /// Actions used (`app.` prefix): `settings`, `sign-in`, `open-folder`, `toggle-mount`.
    pub fn new(app: &adw::Application) -> std::rc::Rc<Self> {
        let banner = adw::Banner::new(&tr(
            "No notification area found. On GNOME, enable the “AppIndicator and KStatusNotifierItem Support” extension to get the PDrive icon.",
        ));

        let settings = gtk::Button::builder().icon_name("emblem-system-symbolic").tooltip_text(tr("Settings")).action_name("app.settings").build();
        let header = adw::HeaderBar::new();
        header.pack_end(&settings);

        // Sign-in page.
        let sign_in = gtk::Button::builder().label(tr("Sign in")).halign(gtk::Align::Center).action_name("app.sign-in").build();
        sign_in.add_css_class("suggested-action");
        sign_in.add_css_class("pill");
        let signin_page = adw::StatusPage::builder()
            .icon_name(APP_ID)
            .title(tr("Sign in to Proton Drive"))
            .description(tr("Unofficial third-party application, not affiliated with Proton AG."))
            .child(&sign_in)
            .build();

        // Main page.
        let avatar = adw::Avatar::new(56, None, true);
        let account = gtk::Label::builder().xalign(0.0).hexpand(true).selectable(true).build();
        account.add_css_class("title-4");
        let quota = gtk::Label::builder().xalign(0.0).build();
        quota.add_css_class("dim-label");
        let texts = gtk::Box::builder().orientation(gtk::Orientation::Vertical).valign(gtk::Align::Center).spacing(2).hexpand(true).build();
        texts.append(&account);
        texts.append(&quota);
        let identity = gtk::Box::builder().spacing(12).build();
        identity.append(&avatar);
        identity.append(&texts);

        let list = gtk::ListBox::builder().selection_mode(gtk::SelectionMode::None).build();
        list.add_css_class("boxed-list");
        let empty = gtk::Label::builder().label(tr("No recent activity")).margin_top(24).margin_bottom(24).build();
        empty.add_css_class("dim-label");
        list.set_placeholder(Some(&empty));
        let scroller = gtk::ScrolledWindow::builder().vexpand(true).hscrollbar_policy(gtk::PolicyType::Never).child(&list).build();

        let open_folder = gtk::Button::builder().label(tr("Open folder")).action_name("app.open-folder").hexpand(true).build();
        open_folder.add_css_class("suggested-action");
        let view_online = gtk::Button::builder().label(tr("View online")).hexpand(true).build();
        view_online.connect_clicked(|button| {
            let window = button.root().and_downcast::<gtk::Window>();
            gtk::UriLauncher::new(WEB_URL).launch(window.as_ref(), gtk::gio::Cancellable::NONE, |result| {
                if let Err(e) = result {
                    eprintln!("cannot open {WEB_URL}: {e}");
                }
            });
        });
        let buttons = gtk::Box::builder().spacing(12).homogeneous(true).build();
        buttons.append(&open_folder);
        buttons.append(&view_online);

        let main = gtk::Box::builder().orientation(gtk::Orientation::Vertical).spacing(18).margin_top(18).margin_bottom(18).margin_start(18).margin_end(18).build();
        main.append(&identity);
        main.append(&scroller);
        main.append(&buttons);

        let stack = gtk::Stack::new();
        stack.add_named(&signin_page, Some("signin"));
        stack.add_named(&main, Some("main"));

        // State bar.
        let status = gtk::Label::builder().xalign(0.0).hexpand(true).build();
        let spinner = gtk::Spinner::new();
        let mount = gtk::Button::builder().action_name("app.toggle-mount").build();
        mount.add_css_class("flat");
        let bar = gtk::Box::builder().spacing(8).margin_top(6).margin_bottom(6).margin_start(12).margin_end(12).build();
        bar.append(&spinner);
        bar.append(&status);
        bar.append(&mount);

        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.add_top_bar(&banner);
        toolbar.set_content(Some(&stack));
        toolbar.add_bottom_bar(&bar);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("PDrive")
            .icon_name(APP_ID)
            .default_width(420)
            .default_height(600)
            .hide_on_close(true)
            .content(&toolbar)
            .build();

        let this = std::rc::Rc::new(Self {
            window,
            banner,
            stack,
            sign_in,
            avatar,
            account,
            quota,
            list,
            status,
            spinner,
            mount,
            activity: RefCell::new(Vec::new()),
        });
        // Relative times age while the window stays open.
        let weak = std::rc::Rc::downgrade(&this);
        glib::timeout_add_seconds_local(30, move || {
            let Some(this) = weak.upgrade() else { return glib::ControlFlow::Break };
            if this.window.is_visible() {
                this.fill_list();
            }
            glib::ControlFlow::Continue
        });
        this
    }

    pub fn set_notification_area_missing(&self, missing: bool) {
        self.banner.set_revealed(missing);
    }

    pub fn set_signing_in(&self, waiting: bool) {
        self.sign_in.set_sensitive(!waiting);
        self.sign_in.set_label(&if waiting { tr("Waiting for the browser…") } else { tr("Sign in") });
    }

    pub fn update(&self, view: &View) {
        let signed_out = view.status == Status::SignedOut;
        self.stack.set_visible_child_name(if signed_out { "signin" } else { "main" });
        self.status.set_label(&view.status.label());
        self.spinner.set_spinning(matches!(view.status, Status::Syncing | Status::Starting));
        self.spinner.set_visible(self.spinner.is_spinning());
        match view.status {
            Status::Unmounted => {
                self.mount.set_label(&tr("Mount"));
                self.mount.set_visible(true);
            }
            Status::Idle | Status::Syncing | Status::Error => {
                self.mount.set_label(&tr("Unmount"));
                self.mount.set_visible(true);
            }
            _ => self.mount.set_visible(false),
        }
        if !view.account.is_empty() {
            self.account.set_label(&view.account);
            // Initials of "first.last@example.com": "FL".
            let local = view.account.split('@').next().unwrap_or_default().replace(['.', '_', '-'], " ");
            self.avatar.set_text(Some(&local));
        }
        self.quota.set_label(&quota_text(view.used_space, view.max_space));
        self.quota.set_visible(view.max_space > 0 || view.used_space > 0);
    }

    pub fn set_activity(&self, entries: Vec<Activity>) {
        *self.activity.borrow_mut() = entries;
        self.fill_list();
    }

    fn fill_list(&self) {
        self.list.remove_all();
        for entry in self.activity.borrow().iter() {
            self.list.append(&activity_row(entry));
        }
    }
}

fn quota_text(used: u64, max: u64) -> String {
    let (used, max) = (glib::format_size(used), glib::format_size(max));
    if max.starts_with("0 ") { used.to_string() } else { tr_fmt("{} used of {}", &[&used, &max]) }
}

fn activity_row(entry: &Activity) -> adw::ActionRow {
    let name = entry.path.rsplit('/').next().unwrap_or(&entry.path);
    let running = entry.state == "running";
    let what = describe(&entry.kind, &entry.state);
    let mut details = vec![what];
    if !running {
        details.push(relative_time(entry.timestamp));
    }
    if entry.size > 0 {
        details.push(glib::format_size(entry.size).to_string());
    }
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(name)).subtitle(glib::markup_escape_text(&details.join(" · "))).tooltip_text(&entry.path).build();
    row.add_prefix(&gtk::Image::from_icon_name(match entry.kind.as_str() {
        "upload" => "go-up-symbolic",
        "download" => "go-down-symbolic",
        "trash" | "remote-removed" => "user-trash-symbolic",
        "rename" | "move" | "remote-renamed" => "document-edit-symbolic",
        _ => "emblem-synchronizing-symbolic",
    }));
    if running {
        row.add_suffix(&gtk::Spinner::builder().spinning(true).build());
    } else if entry.state == "failed" {
        row.add_suffix(&gtk::Image::from_icon_name("dialog-error-symbolic"));
    }
    row
}

fn describe(kind: &str, state: &str) -> String {
    match (kind, state) {
        ("upload", "running") => tr("Uploading…"),
        ("upload", "failed") => tr("Upload failed"),
        ("upload", _) => tr("Uploaded"),
        ("download", "running") => tr("Downloading…"),
        ("download", "failed") => tr("Download failed"),
        ("download", _) => tr("Downloaded"),
        ("rename", _) => tr("Renamed"),
        ("move", _) => tr("Moved"),
        ("trash", _) => tr("Moved to trash"),
        ("remote-created", _) => tr("Added online"),
        ("remote-renamed", _) => tr("Renamed online"),
        ("remote-updated", _) => tr("Updated online"),
        ("remote-removed", _) => tr("Removed online"),
        _ => kind.to_owned(),
    }
}

fn relative_time(timestamp: i64) -> String {
    let elapsed = glib::real_time() / 1_000_000 - timestamp;
    match elapsed {
        ..60 => tr("Just now"),
        60..3600 => tr_n("{} minute ago", "{} minutes ago", (elapsed / 60) as u32, &[&(elapsed / 60).to_string()]),
        3600..86400 => tr_n("{} hour ago", "{} hours ago", (elapsed / 3600) as u32, &[&(elapsed / 3600).to_string()]),
        _ => tr_n("{} day ago", "{} days ago", (elapsed / 86400) as u32, &[&(elapsed / 86400).to_string()]),
    }
}
