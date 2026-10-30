// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Settings dialog.

use std::rc::Rc;

use adw::prelude::*;

use crate::config::{self, Settings};
use pdi18n::tr;

/// Shows the settings. The mount point and the cache size are saved when the dialog closes, and
/// `restart_daemon` then applies them; `sign_out` is called after the user confirms.
pub fn show(parent: &impl IsA<gtk::Widget>, account: &str, restart_daemon: Rc<dyn Fn()>, sign_out: Rc<dyn Fn()>) {
    let current = config::load();
    let dialog = adw::PreferencesDialog::builder().title(tr("Settings")).build();
    let page = adw::PreferencesPage::new();

    let folder = adw::PreferencesGroup::builder()
        .title(tr("Synchronization"))
        .description(tr("Changes apply when the dialog is closed, restarting the service."))
        .build();
    let mountpoint = adw::EntryRow::builder().title(tr("Mount point")).text(&current.mountpoint).build();
    let cache = adw::SpinRow::with_range(1.0, 1000.0, 1.0);
    cache.set_title(&tr("Cache size (GiB)"));
    cache.set_digits(1);
    cache.set_value(current.cache_size_gib.clamp(1.0, 1000.0));
    folder.add(&mountpoint);
    folder.add(&cache);
    page.add(&folder);

    let startup = adw::PreferencesGroup::new();
    let autostart = adw::SwitchRow::builder().title(tr("Start when I log in")).active(config::autostart_enabled()).build();
    let toaster = dialog.clone();
    autostart.connect_active_notify(move |row| {
        if let Err(e) = config::set_autostart(row.is_active()) {
            toaster.add_toast(adw::Toast::new(&format!("{}: {e}", tr("Cannot change the startup setting"))));
        }
    });
    startup.add(&autostart);
    let show_window = adw::SwitchRow::builder().title(tr("Show the status window at startup")).active(config::show_window_at_startup()).build();
    let toaster = dialog.clone();
    show_window.connect_active_notify(move |row| {
        if let Err(e) = config::set_show_window_at_startup(row.is_active()) {
            toaster.add_toast(adw::Toast::new(&format!("{}: {e}", tr("Cannot change the startup setting"))));
        }
    });
    startup.add(&show_window);
    page.add(&startup);

    let account_group = adw::PreferencesGroup::builder().title(tr("Account")).build();
    let row = adw::ActionRow::builder().title(if account.is_empty() { "Proton Drive".to_owned() } else { account.to_owned() }).use_markup(false).build();
    let sign_out_button = gtk::Button::builder().label(tr("Sign out")).valign(gtk::Align::Center).build();
    sign_out_button.add_css_class("destructive-action");
    let confirm_parent = dialog.clone();
    sign_out_button.connect_clicked(move |_| {
        let confirm = adw::AlertDialog::new(
            Some(&tr("Sign out?")),
            Some(&tr("The service is stopped and the session is removed from this computer. Pending uploads are resumed after the next sign-in.")),
        );
        confirm.add_responses(&[("cancel", &tr("Cancel")), ("sign-out", &tr("Sign out"))]);
        confirm.set_response_appearance("sign-out", adw::ResponseAppearance::Destructive);
        confirm.set_default_response(Some("cancel"));
        confirm.set_close_response("cancel");
        let (sign_out, dialog) = (sign_out.clone(), confirm_parent.clone());
        confirm.connect_response(None, move |_, response| {
            if response == "sign-out" {
                dialog.close();
                sign_out();
            }
        });
        confirm.present(Some(&confirm_parent));
    });
    row.add_suffix(&sign_out_button);
    account_group.add(&row);
    page.add(&account_group);

    dialog.add(&page);
    // A shown dialog focuses its first field, which selects the mount point: the page takes the
    // initial focus instead, and takes it back from a text field when clicking outside of it.
    page.set_focusable(true);
    dialog.set_focus(Some(&page));
    let click = gtk::GestureClick::new();
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed(move |gesture, _, x, y| {
        let Some(dialog) = gesture.widget() else { return };
        let Some(focus) = dialog.root().and_then(|root| root.focus()).filter(|focus| focus.is::<gtk::Text>()) else { return };
        let field = focus.ancestor(adw::PreferencesRow::static_type()).unwrap_or(focus);
        if !dialog.pick(x, y, gtk::PickFlags::DEFAULT).is_some_and(|target| target == field || target.is_ancestor(&field)) {
            page.grab_focus();
        }
    });
    dialog.add_controller(click);
    dialog.connect_closed(move |_| {
        let mountpoint = mountpoint.text().trim().to_owned();
        let size = cache.value();
        if mountpoint.is_empty() || (mountpoint == current.mountpoint && size == current.cache_size_gib) {
            return;
        }
        match config::save(&Settings { mountpoint, cache_size_gib: size }) {
            Ok(()) => restart_daemon(),
            Err(e) => eprintln!("cannot save the settings: {e:#}"),
        }
    });
    dialog.present(Some(parent));
}
