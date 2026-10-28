//! Translations: gettext, domain `pdrive`. Texts are written in English in the code, through
//! `tr`, `tr_fmt` and `tr_n` (the keywords of `scripts/update-po.sh`).

use std::path::{Path, PathBuf};

use gettextrs::{bind_textdomain_codeset, bindtextdomain, gettext, ngettext, setlocale, textdomain, LocaleCategory};

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
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().any(|e| e.path().join("LC_MESSAGES").join(format!("{DOMAIN}.mo")).is_file()))
        .unwrap_or(false)
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

fn fill(mut text: String, args: &[&str]) -> String {
    for arg in args {
        text = text.replacen("{}", arg, 1);
    }
    text
}
