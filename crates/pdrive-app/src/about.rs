// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! About dialog.

use adw::prelude::*;

use crate::config::APP_ID;
use pdi18n::{tr, tr_fmt};

/// Shows the application icon, name, description and version.
pub fn show(parent: &impl IsA<gtk::Widget>) {
    let icon = gtk::Image::builder().icon_name(APP_ID).pixel_size(128).build();
    let name = gtk::Label::new(Some("PDrive for Linux"));
    name.add_css_class("title-1");
    let description = gtk::Label::builder()
        .label(tr("Unofficial Proton Drive client for Linux, integrated with the desktop."))
        .wrap(true)
        .justify(gtk::Justification::Center)
        .build();
    let version = gtk::Label::new(Some(&tr_fmt("Version {}", &[env!("CARGO_PKG_VERSION")])));
    version.add_css_class("dim-label");

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .halign(gtk::Align::Center)
        .margin_top(12)
        .margin_bottom(24)
        .margin_start(24)
        .margin_end(24)
        .build();
    for widget in [icon.upcast_ref::<gtk::Widget>(), name.upcast_ref(), description.upcast_ref(), version.upcast_ref()] {
        content.append(widget);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let dialog = adw::Dialog::builder().title(tr("About")).content_width(360).child(&toolbar).build();
    dialog.present(Some(parent));
}
