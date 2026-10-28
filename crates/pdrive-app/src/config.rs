// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Settings file shared with the daemon (`$XDG_CONFIG_HOME/pdrive/config.toml`, see
//! `crates/pdrived/src/config.rs`), edited while preserving comments and layout, and the XDG
//! autostart entry.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Value};

pub const APP_ID: &str = "io.github.bpresles.PDrive.App";

pub struct Settings {
    pub mountpoint: String,
    pub cache_size_gib: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self { mountpoint: "~/ProtonDrive".into(), cache_size_gib: 10.0 }
    }
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

fn config_home() -> PathBuf {
    match std::env::var_os("XDG_CONFIG_HOME") {
        Some(dir) if !dir.is_empty() && Path::new(&dir).is_absolute() => PathBuf::from(dir),
        _ => home().join(".config"),
    }
}

fn settings_path() -> PathBuf {
    config_home().join("pdrive").join("config.toml")
}

fn read_document() -> DocumentMut {
    parse(&std::fs::read_to_string(settings_path()).unwrap_or_default())
}

fn parse(text: &str) -> DocumentMut {
    text.parse().unwrap_or_default()
}

pub fn load() -> Settings {
    settings_of(&read_document())
}

fn settings_of(document: &DocumentMut) -> Settings {
    let defaults = Settings::default();
    Settings {
        mountpoint: document.get("mountpoint").and_then(|v| v.as_str()).map_or(defaults.mountpoint, str::to_owned),
        cache_size_gib: document
            .get("cache_size_gib")
            .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
            .unwrap_or(defaults.cache_size_gib),
    }
}

fn write_document(document: &DocumentMut) -> Result<()> {
    let path = settings_path();
    std::fs::create_dir_all(path.parent().context("no parent directory")?)?;
    std::fs::write(&path, document.to_string()).with_context(|| format!("cannot write {}", path.display()))
}

/// Replaces a value keeping the comment that follows it.
fn set(document: &mut DocumentMut, key: &str, mut value: Value) {
    if let Some(old) = document.get(key).and_then(|item| item.as_value()) {
        *value.decor_mut() = old.decor().clone();
    }
    document[key] = toml_edit::Item::Value(value);
}

pub fn save(settings: &Settings) -> Result<()> {
    let mut document = read_document();
    apply(&mut document, settings);
    write_document(&document)
}

fn apply(document: &mut DocumentMut, settings: &Settings) {
    set(document, "mountpoint", Value::from(settings.mountpoint.as_str()));
    let size = if settings.cache_size_gib.fract() == 0.0 { Value::from(settings.cache_size_gib as i64) } else { Value::from(settings.cache_size_gib) };
    set(document, "cache_size_gib", size);
}

/// Whether a launch (other than `--background`) opens the status window; on by default.
pub fn show_window_at_startup() -> bool {
    show_window_of(&read_document())
}

fn show_window_of(document: &DocumentMut) -> bool {
    document.get("show_window_at_startup").and_then(|v| v.as_bool()).unwrap_or(true)
}

pub fn set_show_window_at_startup(show: bool) -> Result<()> {
    let mut document = read_document();
    set(&mut document, "show_window_at_startup", Value::from(show));
    write_document(&document)
}

/// Expands a leading "~" to the home folder.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) => home().join(rest.trim_start_matches('/')),
        None => PathBuf::from(path),
    }
}

fn autostart_path() -> PathBuf {
    config_home().join("autostart").join(format!("{APP_ID}.desktop"))
}

/// Application id before the definitive D-Bus identifier; only used by the migration below.
const LEGACY_APP_ID: &str = "io.github.pdrive.PDrive.App";

/// Renames an autostart entry made under the former application id, so that the automatic
/// start stays enabled. The `Icon` line is rewritten with the new id. Does nothing when
/// the new entry already exists.
pub fn migrate_legacy_autostart() {
    let legacy = config_home().join("autostart").join(format!("{LEGACY_APP_ID}.desktop"));
    let Ok(text) = std::fs::read_to_string(&legacy) else { return };
    let path = autostart_path();
    if !path.exists()
        && let Err(e) = std::fs::write(&path, text.replace(LEGACY_APP_ID, APP_ID))
    {
        eprintln!("cannot migrate the autostart entry to {}: {e}", path.display());
        return;
    }
    let _ = std::fs::remove_file(&legacy);
}

pub fn autostart_enabled() -> bool {
    autostart_path().exists()
}

pub fn set_autostart(enabled: bool) -> Result<()> {
    let path = autostart_path();
    if !enabled {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        };
    }
    let exe = std::env::current_exe().context("cannot find the executable")?;
    // Quoted as the Desktop Entry specification requires for paths with spaces.
    let exec = format!("\"{}\" --background", exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\""));
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=PDrive\nComment=Proton Drive mount (unofficial third-party client)\nExec={exec}\nIcon={APP_ID}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n"
    );
    std::fs::create_dir_all(path.parent().context("no parent directory")?)?;
    std::fs::write(&path, entry).with_context(|| format!("cannot write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMENTED: &str = "# Where \"My files\" is mounted.\nmountpoint = \"~/ProtonDrive\" # trailing\n\n# Size of the cache.\ncache_size_gib = 10\n# Window.\nshow_window_at_startup = false\n";

    #[test]
    fn defaults_when_the_file_is_empty_or_invalid() {
        for text in ["", "this is = not [valid"] {
            let settings = settings_of(&parse(text));
            assert_eq!(settings.mountpoint, "~/ProtonDrive");
            assert_eq!(settings.cache_size_gib, 10.0);
            assert!(show_window_of(&parse(text)));
        }
    }

    #[test]
    fn show_window_at_startup_defaults_to_true_and_reads_false() {
        assert!(show_window_of(&parse("mountpoint = \"x\"\n")));
        assert!(!show_window_of(&parse("show_window_at_startup = false\n")));
        assert!(show_window_of(&parse("show_window_at_startup = true\n")));
        // A value of the wrong type is ignored.
        assert!(show_window_of(&parse("show_window_at_startup = \"no\"\n")));
    }

    #[test]
    fn reads_integer_and_float_sizes() {
        assert_eq!(settings_of(&parse("cache_size_gib = 4\n")).cache_size_gib, 4.0);
        assert_eq!(settings_of(&parse("cache_size_gib = 0.5\n")).cache_size_gib, 0.5);
        assert_eq!(settings_of(&parse("mountpoint = \"/mnt/x\"\n")).mountpoint, "/mnt/x");
    }

    #[test]
    fn saving_preserves_comments_layout_and_other_keys() {
        let mut document = parse(COMMENTED);
        apply(&mut document, &Settings { mountpoint: "/mnt/drive".into(), cache_size_gib: 2.5 });
        let text = document.to_string();
        assert_eq!(
            text,
            "# Where \"My files\" is mounted.\nmountpoint = \"/mnt/drive\" # trailing\n\n# Size of the cache.\ncache_size_gib = 2.5\n# Window.\nshow_window_at_startup = false\n"
        );
        assert!(!show_window_of(&parse(&text)));
    }

    #[test]
    fn whole_sizes_are_written_as_integers() {
        let mut document = parse("");
        apply(&mut document, &Settings { mountpoint: "~/D".into(), cache_size_gib: 20.0 });
        assert_eq!(document.to_string(), "mountpoint = \"~/D\"\ncache_size_gib = 20\n");
        let settings = settings_of(&parse(&document.to_string()));
        assert_eq!((settings.mountpoint.as_str(), settings.cache_size_gib), ("~/D", 20.0));
    }

    #[test]
    fn setting_the_window_flag_keeps_its_comment() {
        let mut document = parse(COMMENTED);
        set(&mut document, "show_window_at_startup", Value::from(true));
        assert!(document.to_string().contains("# Window.\nshow_window_at_startup = true\n"));
        assert!(show_window_of(&document));
        // Added when absent.
        let mut document = parse("mountpoint = \"x\"\n");
        set(&mut document, "show_window_at_startup", Value::from(false));
        assert!(!show_window_of(&parse(&document.to_string())));
        assert!(document.to_string().starts_with("mountpoint = \"x\"\n"));
    }
}
