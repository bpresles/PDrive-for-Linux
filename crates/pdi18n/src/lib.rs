// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Translations: gettext, domain `pdrive`. Texts are written in English in the code, through
//! `tr`, `tr_fmt` and `tr_n` (the keywords of `scripts/update-po.sh`).

use std::path::{Path, PathBuf};

use gettextrs::{LocaleCategory, bind_textdomain_codeset, bindtextdomain, gettext, ngettext, setlocale, textdomain};

pub const DOMAIN: &str = "pdrive";

/// Where the catalogues are looked up when the executable is not installed in a prefix with
/// them (override at build time with the PDRIVE_LOCALEDIR environment variable).
const DEFAULT_LOCALEDIR: &str = match option_env!("PDRIVE_LOCALEDIR") {
    Some(dir) => dir,
    None => "/usr/share/locale",
};

/// Binds the `pdrive` domain to its catalogues. A missing catalogue only means that the English
/// texts are shown.
pub fn init() {
    setlocale(LocaleCategory::LcAll, "");
    let _ = bindtextdomain(DOMAIN, locale_dir());
    let _ = bind_textdomain_codeset(DOMAIN, "UTF-8");
    let _ = textdomain(DOMAIN);
}

/// Catalogue directory, by priority: PDRIVE_LOCALEDIR at run time (development), `share/locale`
/// of the prefix of the executable (`~/.local/bin/x` gives `~/.local/share/locale`) when it holds
/// a PDrive catalogue, then the build-time default.
fn locale_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("PDRIVE_LOCALEDIR") {
        return PathBuf::from(dir);
    }
    let from_exe = std::env::current_exe().ok().and_then(|exe| Some(exe.parent()?.parent()?.join("share/locale")));
    match from_exe {
        Some(dir) if has_catalogue(&dir) => dir,
        _ => PathBuf::from(DEFAULT_LOCALEDIR),
    }
}

fn has_catalogue(dir: &Path) -> bool {
    std::fs::read_dir(dir).map(|entries| entries.flatten().any(|e| e.path().join("LC_MESSAGES").join(format!("{DOMAIN}.mo")).is_file())).unwrap_or(false)
}

pub fn tr(message: &str) -> String {
    gettext(message)
}

/// Translates a message and replaces its `{}` placeholders with `args`, in order.
pub fn tr_fmt(message: &str, args: &[&str]) -> String {
    fill(gettext(message), args)
}

/// Translates a message with plural forms for `n`; the `{}` placeholders are replaced like in
/// `tr_fmt` (`args` usually holds `n`).
pub fn tr_n(singular: &str, plural: &str, n: u32, args: &[&str]) -> String {
    fill(ngettext(singular, plural, n), args)
}

fn fill(text: String, args: &[&str]) -> String {
    // One pass: a placeholder inside an argument (e.g. a file name) is never substituted again.
    let mut result = String::with_capacity(text.len());
    let mut args = args.iter();
    let mut rest = text.as_str();
    while let Some(position) = rest.find("{}") {
        let Some(arg) = args.next() else { break };
        result.push_str(&rest[..position]);
        result.push_str(arg);
        rest = &rest[position + 2..];
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_placeholders_in_order() {
        assert_eq!(fill("{} of {}".into(), &["1", "2"]), "1 of 2");
        assert_eq!(fill("no placeholder".into(), &["x"]), "no placeholder");
        assert_eq!(fill(String::new(), &[]), "");
    }

    #[test]
    fn fill_keeps_unmatched_placeholders_and_ignores_extra_arguments() {
        assert_eq!(fill("{} and {}".into(), &["a"]), "a and {}");
        assert_eq!(fill("{}".into(), &["a", "b"]), "a");
    }

    #[test]
    fn fill_does_not_substitute_inside_arguments() {
        assert_eq!(fill("{} / {}".into(), &["{}", "x"]), "{} / x");
    }

    #[test]
    fn fill_keeps_multibyte_text() {
        assert_eq!(fill("é {} ü".into(), &["→"]), "é → ü");
    }

    // Without a bound catalogue, gettext returns the English text.
    #[test]
    fn tr_fmt_formats_the_untranslated_message() {
        assert_eq!(tr("Hello"), "Hello");
        assert_eq!(tr_fmt("Moved {} to {}", &["a", "b"]), "Moved a to b");
    }

    #[test]
    fn tr_n_picks_the_plural_form() {
        let text = |n: u32| tr_n("{} file", "{} files", n, &[&n.to_string()]);
        assert_eq!(text(0), "0 files");
        assert_eq!(text(1), "1 file");
        assert_eq!(text(2), "2 files");
    }
}
