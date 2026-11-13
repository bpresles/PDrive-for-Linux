// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
//! Integration of the drive in the file managers, through the user's own configuration files
//! (a package cannot write them):
//!
//! - Dolphin (and the KDE file dialogs): an "Proton Drive" entry in `user-places.xbel`, marked
//!   with `<ID>pdrive-for-linux</ID>`. Only an existing file is changed: KDE would stop adding
//!   its default places to a file made by someone else.
//! - COSMIC Files: a favorite (`Path("…")`) and two context-menu actions running
//!   `pdrive keep-offline` / `pdrive free-up-space` (see `crates/pdrive/src/offline.rs`), only
//!   when `com.system76.CosmicFiles.desktop` is installed. The RON files are edited as text:
//!   a round trip through a RON value would lose the enum variants such as `Path("…")`.
//!
//! What is installed decides, never the current session: both are set up on a machine that
//! has both file managers, whatever desktop runs now. Everything is idempotent, only touches
//! the entries recognised as ours (the XBEL marker, the `pdrive` command of an action, the
//! mount point of a favorite), keeps the rest of the files as they are and writes only when
//! the content changes, atomically.
//! Nautilus needs nothing here: its sidebar entry comes from libcloudproviders.

use std::ops::Range;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use pdi18n::tr;

use crate::config;
use crate::supervisor::program;

const XBEL_MARKER: &str = "<ID>pdrive-for-linux</ID>";
const XBEL_TITLE: &str = "Proton Drive";
const XBEL_ICON: &str = config::APP_ID;
const COSMIC_FILES_DESKTOP: &str = "com.system76.CosmicFiles.desktop";
const COSMIC_CONFIG: &str = "cosmic/com.system76.CosmicFiles/v1";
const COSMIC_DEFAULT_FAVORITES: [&str; 6] = ["Home", "Documents", "Downloads", "Music", "Pictures", "Videos"];

/// Where things are: the XDG directories of the user and the system.
#[derive(Clone, Debug)]
pub struct Env {
    pub data_home: PathBuf,
    pub data_dirs: Vec<PathBuf>,
    pub config_home: PathBuf,
}

impl Env {
    pub fn from_process() -> Self {
        let data_dirs = match std::env::var("XDG_DATA_DIRS") {
            Ok(dirs) if !dirs.is_empty() => dirs.split(':').map(PathBuf::from).collect(),
            _ => vec![PathBuf::from("/usr/local/share"), PathBuf::from("/usr/share")],
        };
        Self { data_home: config::data_home(), data_dirs, config_home: config::config_home() }
    }

    fn desktop_file_installed(&self, id: &str) -> bool {
        std::iter::once(&self.data_home).chain(&self.data_dirs).any(|dir| dir.join("applications").join(id).is_file())
    }

    fn xbel(&self) -> PathBuf {
        self.data_home.join("user-places.xbel")
    }

    fn cosmic_dir(&self) -> PathBuf {
        self.config_home.join(COSMIC_CONFIG)
    }
}

/// The two context-menu actions, with their names in the language of the user.
pub struct Actions {
    pub keep_name: String,
    pub free_name: String,
    /// The `pdrive` command: `pdrive` when the PATH of the session finds it, else its full path.
    pub command: String,
}

impl Actions {
    pub fn current() -> Self {
        Self { keep_name: tr("PDrive: Keep Available Offline"), free_name: tr("PDrive: Free Up Space"), command: pdrive_command() }
    }

    fn entries(&self) -> [(Subcommand, String); 2] {
        [(Subcommand::KeepOffline, self.entry(Subcommand::KeepOffline)), (Subcommand::FreeUpSpace, self.entry(Subcommand::FreeUpSpace))]
    }

    fn entry(&self, subcommand: Subcommand) -> String {
        let (name, word) = match subcommand {
            Subcommand::KeepOffline => (&self.keep_name, "keep-offline"),
            Subcommand::FreeUpSpace => (&self.free_name, "free-up-space"),
        };
        let line = format!("{} {word} %F", quote_word(&self.command));
        format!("(name: {}, confirm: false, selection: Any, steps: [{}])", ron_string(name), ron_string(&line))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Subcommand {
    KeepOffline,
    FreeUpSpace,
}

/// `pdrive` when the PATH finds an executable of that name, else the one installed next to
/// this program (`~/.local/bin` is not always in the PATH of a graphical session).
fn pdrive_command() -> String {
    let in_path = std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| is_executable(&dir.join("pdrive"))));
    let next_to_us = program("pdrive");
    if in_path || !next_to_us.is_absolute() { "pdrive".to_owned() } else { next_to_us.to_string_lossy().into_owned() }
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Sets the integrations up (`enabled`) or removes them. `old_mount` is the previous mount
/// point, when it changed: its favorite is removed. Returns the errors, one per file manager:
/// a failure of one does not prevent the others.
pub fn apply(env: &Env, enabled: bool, old_mount: Option<&Path>, mount: &Path, actions: &Actions) -> Vec<anyhow::Error> {
    let mut errors = Vec::new();
    if let Err(e) = apply_dolphin(env, enabled, mount) {
        errors.push(e.context("Dolphin places"));
    }
    if let Err(e) = apply_cosmic(env, enabled, old_mount, mount, actions) {
        errors.push(e.context("COSMIC Files"));
    }
    errors
}

/// Applies the setting `file_manager_integration` for the current mount point (used at startup
/// and when the setting changes).
pub fn apply_from_settings() -> Vec<anyhow::Error> {
    let (env, mount) = (Env::from_process(), config::expand_home(&config::load().mountpoint));
    apply(&env, config::file_manager_integration(), None, &mount, &Actions::current())
}

/// The mount point setting changed (`old` and `new` as written in the settings): the entries
/// follow it, when enabled.
pub fn mount_point_changed(old: &str, new: &str) -> Vec<anyhow::Error> {
    if !config::file_manager_integration() {
        return Vec::new();
    }
    let (old, new) = (config::expand_home(old), config::expand_home(new));
    apply(&Env::from_process(), true, Some(&old), &new, &Actions::current())
}

/// `pdrive-app --remove-integrations`: removes the entries whatever the setting is.
pub fn remove_all() -> bool {
    let (env, mount) = (Env::from_process(), config::expand_home(&config::load().mountpoint));
    let errors = apply(&env, false, None, &mount, &Actions::current());
    let ok = errors.is_empty();
    report(errors);
    ok
}

pub fn report(errors: Vec<anyhow::Error>) {
    for error in errors {
        eprintln!("file manager integration: {error:#}");
    }
}

fn apply_dolphin(env: &Env, enabled: bool, mount: &Path) -> Result<()> {
    // No file, nothing to do (and nothing to remove): retried at the next start.
    update_file(&env.xbel(), false, |text| {
        let text = text.expect("the file exists");
        Ok(if enabled { xbel_add(text, mount) } else { xbel_remove(text) })
    })
}

fn apply_cosmic(env: &Env, enabled: bool, old_mount: Option<&Path>, mount: &Path, actions: &Actions) -> Result<()> {
    // Disabling cleans what may have been written even if the application was removed since.
    if enabled && !env.desktop_file_installed(COSMIC_FILES_DESKTOP) {
        return Ok(());
    }
    let dir = env.cosmic_dir();
    let favorites = update_file(&dir.join("favorites"), enabled, |text| favorites_sync(text, enabled, old_mount, mount));
    let context = update_file(&dir.join("context_actions"), enabled, |text| context_actions_sync(text, enabled, actions));
    favorites.and(context)
}

/// Reads `path`, lets `change` compute the new content (`None`: unchanged) and writes it
/// atomically. A missing file is passed as `None` when `create` is set, else it is skipped.
fn update_file(path: &Path, create: bool, change: impl FnOnce(Option<&str>) -> Result<Option<String>>) -> Result<()> {
    // Dotfile managers link these files: the link is kept, its target is replaced.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let text = match std::fs::read_to_string(&real) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(anyhow!(e).context(format!("cannot read {}", path.display()))),
    };
    if text.is_none() && !create {
        return Ok(());
    }
    let Some(new) = change(text.as_deref()).with_context(|| format!("cannot update {}", path.display()))? else { return Ok(()) };
    if text.as_deref() == Some(new.as_str()) {
        return Ok(());
    }
    write_atomically(&real, &new).with_context(|| format!("cannot write {}", path.display()))
}

fn write_atomically(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().context("no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let name = path.file_name().context("no file name")?.to_string_lossy();
    let temporary = parent.join(format!(".{name}.pdrive-{}.tmp", std::process::id()));
    let written = std::fs::write(&temporary, content).and_then(|()| {
        if let Ok(metadata) = std::fs::metadata(path) {
            let _ = std::fs::set_permissions(&temporary, metadata.permissions());
        }
        std::fs::rename(&temporary, path)
    });
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    Ok(written?)
}

// ---------------------------------------------------------------------------------------------
// XBEL (KDE places)
// ---------------------------------------------------------------------------------------------

/// `file://` URI with the bytes other than unreserved characters and `/` percent-encoded.
fn file_uri(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut uri = String::from("file://");
    for &byte in path.as_os_str().as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/') {
            uri.push(byte as char);
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri
}

fn xbel_bookmark(href: &str) -> String {
    format!(
        " <bookmark href=\"{href}\">\n  <title>{XBEL_TITLE}</title>\n  <info>\n   <metadata owner=\"http://freedesktop.org\">\n    <bookmark:icon name=\"{XBEL_ICON}\"/>\n   </metadata>\n   <metadata owner=\"http://www.kde.org\">\n    {XBEL_MARKER}\n    <isSystemItem>false</isSystemItem>\n   </metadata>\n  </info>\n </bookmark>\n"
    )
}

/// The `<bookmark>` element (whole lines) that holds our marker.
fn xbel_element(text: &str) -> Option<Range<usize>> {
    let marker = text.find(XBEL_MARKER)?;
    // "<bookmark " with its space: `<bookmark:icon` is another element.
    let open = text[..marker].rfind("<bookmark ")?;
    let close = marker + text[marker..].find("</bookmark>")? + "</bookmark>".len();
    let start = text[..open].rfind('\n').map_or(0, |i| i + 1);
    let end = text[close..].find('\n').map_or(text.len(), |i| close + i + 1);
    Some(start..end)
}

/// Adds the entry, or only follows a new mount point in the existing one (so that what the user
/// changed in Dolphin, such as hiding it, stays). `None` when nothing changes or the text is
/// not an XBEL file.
fn xbel_add(text: &str, mount: &Path) -> Option<String> {
    let href = file_uri(mount);
    if let Some(range) = xbel_element(text) {
        let element = &text[range.clone()];
        let marker = element.find("href=\"")? + "href=\"".len();
        let end = marker + element[marker..].find('"')?;
        if element[marker..end] == href {
            return None;
        }
        return Some(format!("{}{}{}{}{}", &text[..range.start], &element[..marker], href, &element[end..], &text[range.end..]));
    }
    let close = text.rfind("</xbel>")?;
    let separator = if close == 0 || text[..close].ends_with('\n') { "" } else { "\n" };
    Some(format!("{}{separator}{}{}", &text[..close], xbel_bookmark(&href), &text[close..]))
}

fn xbel_remove(text: &str) -> Option<String> {
    let range = xbel_element(text)?;
    Some(format!("{}{}", &text[..range.start], &text[range.end..]))
}

// ---------------------------------------------------------------------------------------------
// COSMIC Files (RON, edited as text)
// ---------------------------------------------------------------------------------------------

fn ron_string(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The word as a shell-like command line wants it: quoted when it holds a space or a quote.
fn quote_word(word: &str) -> String {
    if word.chars().any(|c| c.is_whitespace() || c == '"' || c == '\\' || c == '\'') {
        format!("\"{}\"", word.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        word.to_owned()
    }
}

/// Splits a command line on spaces, `"…"` grouping words (with `\` escapes inside).
fn split_command(line: &str) -> Vec<String> {
    let (mut words, mut word, mut quoted, mut started) = (Vec::new(), String::new(), false, false);
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' if quoted => word.extend(chars.next()),
            c if c.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            c => {
                word.push(c);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The string literals of a RON fragment, unescaped.
fn ron_strings(fragment: &str) -> Vec<String> {
    let (mut strings, mut current, mut inside) = (Vec::new(), String::new(), false);
    let mut chars = fragment.chars();
    while let Some(c) = chars.next() {
        match (inside, c) {
            (false, '"') => inside = true,
            (true, '"') => {
                inside = false;
                strings.push(std::mem::take(&mut current));
            }
            (true, '\\') => current.extend(chars.next()),
            (true, c) => current.push(c),
            _ => {}
        }
    }
    strings
}

/// The action of one `context_actions` element when it runs our `pdrive` command.
fn recognise(group: &str) -> Option<Subcommand> {
    // Only the command lines (`steps`), not the name.
    let steps = &group[group.find("steps:")?..];
    ron_strings(steps).iter().find_map(|line| {
        let words = split_command(line);
        let program = Path::new(words.first()?).file_name()?;
        if program != "pdrive" {
            return None;
        }
        match words.get(1)?.as_str() {
            "keep-offline" => Some(Subcommand::KeepOffline),
            "free-up-space" => Some(Subcommand::FreeUpSpace),
            _ => None,
        }
    })
}

/// Positions of the `[` and the `]` that delimit the list of a RON file (the `]` is the last
/// character that is not whitespace), skipping strings and comments.
fn list_bounds(text: &str) -> Option<(usize, usize)> {
    let open = scan(text, 0..text.len(), |_, c, _| c == '[').first().map(|(i, _)| *i)?;
    let close = text.trim_end().len().checked_sub(1).filter(|&i| text.as_bytes()[i] == b']' && i > open)?;
    Some((open, close))
}

/// Calls `visit(index, char, depth)` for each character outside strings and `//` comments, with
/// the nesting depth of `([{` before it; returns those for which `visit` is true.
fn scan(text: &str, range: Range<usize>, mut visit: impl FnMut(usize, char, usize) -> bool) -> Vec<(usize, char)> {
    let (mut found, mut depth, mut in_string, mut escaped, mut comment) = (Vec::new(), 0usize, false, false, false);
    let mut previous_slash = false;
    for (offset, c) in text[range.clone()].char_indices() {
        let index = range.start + offset;
        if comment {
            comment = c != '\n';
            continue;
        }
        if in_string {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => in_string = false,
                _ => {}
            }
            continue;
        }
        if c == '/' && previous_slash {
            comment = true;
            previous_slash = false;
            continue;
        }
        previous_slash = c == '/';
        if c == '"' {
            in_string = true;
            continue;
        }
        if visit(index, c, depth) {
            found.push((index, c));
        }
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    found
}

/// The bare `( … )` elements of the list, as byte ranges.
fn list_groups(text: &str, open: usize, close: usize) -> Vec<Range<usize>> {
    let starts = scan(text, open + 1..close, |_, c, depth| c == '(' && depth == 0);
    let ends = scan(text, open + 1..close, |_, c, depth| c == ')' && depth == 1);
    starts.iter().zip(&ends).map(|((start, _), (end, _))| *start..*end + 1).collect()
}

/// Appends an element at the end of the list.
fn list_append(text: &str, item: &str) -> Result<String> {
    let (_, close) = list_bounds(text).context("not a list")?;
    let head = text[..close].trim_end();
    let comma = if head.ends_with(',') || head.ends_with('[') { "" } else { "," };
    Ok(format!("{head}{comma}\n    {item},\n{}", &text[close..]))
}

/// Removes the range with the whole line(s) it takes, its comma included.
fn remove_element(text: &mut String, range: Range<usize>) {
    let start = text[..range.start].rfind('\n').map_or(0, |i| i + 1);
    let alone_before = text[start..range.start].trim().is_empty();
    let mut end = range.end;
    if text[end..].starts_with(',') {
        end += 1;
    }
    let rest = &text[end..];
    let line_rest = rest.find('\n').unwrap_or(rest.len());
    if alone_before && rest[..line_rest].trim().is_empty() {
        end += (line_rest + 1).min(rest.len());
        text.replace_range(start..end, "");
    } else {
        text.replace_range(range.start..end, "");
    }
}

fn favorite_entry(path: &Path) -> String {
    format!("Path({})", ron_string(&path.to_string_lossy()))
}

fn favorites_sync(text: Option<&str>, enabled: bool, old_mount: Option<&Path>, mount: &Path) -> Result<Option<String>> {
    let entry = favorite_entry(mount);
    let Some(text) = text else {
        if !enabled {
            return Ok(None);
        }
        let defaults: String = COSMIC_DEFAULT_FAVORITES.iter().map(|name| format!("    {name},\n")).collect();
        return Ok(Some(format!("[\n{defaults}    {entry},\n]")));
    };
    list_bounds(text).context("not a list")?;
    let mut text = text.to_owned();
    for old in old_mount.into_iter().filter(|old| *old != mount) {
        text = without_lines(&text, &favorite_entry(old));
    }
    if !enabled {
        return Ok(Some(without_lines(&text, &entry)));
    }
    if !text.contains(&entry) {
        text = list_append(&text, &entry)?;
    }
    Ok(Some(text))
}

/// The text without the lines that only hold `entry` (with or without its comma).
fn without_lines(text: &str, entry: &str) -> String {
    text.split_inclusive('\n').filter(|line| line.trim().trim_end_matches(',') != entry).collect()
}

fn context_actions_sync(text: Option<&str>, enabled: bool, actions: &Actions) -> Result<Option<String>> {
    let wanted = actions.entries();
    let Some(text) = text else {
        if !enabled {
            return Ok(None);
        }
        let items: String = wanted.iter().map(|(_, entry)| format!("    {entry},\n")).collect();
        return Ok(Some(format!("[\n{items}]")));
    };
    let (open, close) = list_bounds(text).context("not a list")?;
    let mut text = text.to_owned();
    // Existing elements of ours, from the end so that the ranges stay valid.
    let mut present = Vec::new();
    for range in list_groups(&text, open, close).into_iter().rev() {
        let Some(subcommand) = recognise(&text[range.clone()]) else { continue };
        let entry = &wanted.iter().find(|(s, _)| *s == subcommand).expect("both subcommands").1;
        // Kept (and updated) once per action; the extra copies and all of them when disabled go.
        if enabled && !present.contains(&subcommand) {
            present.push(subcommand);
            if &text[range.clone()] != entry {
                text.replace_range(range, entry);
            }
        } else {
            remove_element(&mut text, range);
        }
    }
    if enabled {
        for (subcommand, entry) in &wanted {
            if !present.contains(subcommand) {
                text = list_append(&text, entry)?;
            }
        }
    }
    Ok(Some(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount() -> PathBuf {
        PathBuf::from("/home/u/ProtonDrive")
    }

    fn actions() -> Actions {
        Actions { keep_name: "PDrive: Keep Available Offline".into(), free_name: "PDrive: Free Up Space".into(), command: "pdrive".into() }
    }

    const XBEL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xbel>\n<xbel>\n <info>\n  <metadata owner=\"http://www.kde.org\">\n   <kde_places_version>4</kde_places_version>\n  </metadata>\n </info>\n <bookmark href=\"file:///home/u\">\n  <title>Home</title>\n  <info>\n   <metadata owner=\"http://freedesktop.org\">\n    <bookmark:icon name=\"user-home\"/>\n   </metadata>\n   <metadata owner=\"http://www.kde.org\">\n    <ID>1/0</ID>\n    <isSystemItem>true</isSystemItem>\n   </metadata>\n  </info>\n </bookmark>\n</xbel>\n";

    // XBEL

    #[test]
    fn xbel_entry_is_added_before_the_end_and_keeps_the_rest() {
        let added = xbel_add(XBEL, &mount()).unwrap();
        assert!(added.starts_with(&XBEL[..XBEL.len() - "</xbel>\n".len()]));
        assert!(added.ends_with(" </bookmark>\n</xbel>\n"));
        assert!(added.contains("<bookmark href=\"file:///home/u/ProtonDrive\">"));
        assert!(added.contains("<title>Proton Drive</title>"));
        assert!(added.contains("<bookmark:icon name=\"io.github.bpresles.PDrive.App\"/>"));
        assert_eq!(added.matches(XBEL_MARKER).count(), 1);
        assert_eq!(added.matches("<bookmark ").count(), 2);
    }

    #[test]
    fn xbel_add_is_idempotent_and_remove_restores_the_file() {
        let added = xbel_add(XBEL, &mount()).unwrap();
        assert_eq!(xbel_add(&added, &mount()), None);
        assert_eq!(xbel_remove(&added).as_deref(), Some(XBEL));
        assert_eq!(xbel_remove(XBEL), None);
    }

    #[test]
    fn xbel_follows_a_new_mount_point_and_keeps_user_changes() {
        let added = xbel_add(XBEL, &mount()).unwrap().replace("<isSystemItem>false", "<IsHidden>true</IsHidden>\n    <isSystemItem>false");
        let moved = xbel_add(&added, Path::new("/mnt/Mon Drive")).unwrap();
        assert!(moved.contains("<bookmark href=\"file:///mnt/Mon%20Drive\">"));
        assert!(moved.contains("<IsHidden>true</IsHidden>"));
        assert_eq!(moved.matches(XBEL_MARKER).count(), 1);
        assert_eq!(moved.replace("Mon%20Drive", "ProtonDrive").replace("/mnt/", "/home/u/"), added);
    }

    #[test]
    fn xbel_without_end_tag_is_left_alone() {
        assert_eq!(xbel_add("not xml", &mount()), None);
    }

    #[test]
    fn file_uri_encodes_unusual_bytes() {
        assert_eq!(file_uri(Path::new("/home/é u/a&b")), "file:///home/%C3%A9%20u/a%26b");
    }

    // RON helpers

    #[test]
    fn command_lines_round_trip() {
        assert_eq!(quote_word("pdrive"), "pdrive");
        assert_eq!(quote_word("/home/a b/pdrive"), "\"/home/a b/pdrive\"");
        assert_eq!(split_command("\"/home/a b/pdrive\" keep-offline %F"), ["/home/a b/pdrive", "keep-offline", "%F"]);
        assert_eq!(ron_strings(r#"(name: "a \"b\"", steps: ["x y"])"#), ["a \"b\"", "x y"]);
    }

    #[test]
    fn our_actions_are_recognised_by_their_command() {
        assert_eq!(recognise(r#"(name: "x", steps: ["pdrive keep-offline %F"])"#), Some(Subcommand::KeepOffline));
        assert_eq!(recognise(r#"(name: "x", steps: ["/opt/b/pdrive free-up-space %F"])"#), Some(Subcommand::FreeUpSpace));
        assert_eq!(recognise(r#"(name: "pdrive keep-offline", steps: ["echo %F"])"#), None);
        assert_eq!(recognise(r#"(name: "x", steps: ["other keep-offline %F"])"#), None);
    }

    // favorites

    const FAVORITES: &str = "[\n    Home,\n    Documents,\n    Path(\"/mnt/data\"),\n    Path(\"/a]b\"),\n]";

    #[test]
    fn favorite_is_appended_once_and_removed() {
        let added = favorites_sync(Some(FAVORITES), true, None, &mount()).unwrap().unwrap();
        assert_eq!(added, "[\n    Home,\n    Documents,\n    Path(\"/mnt/data\"),\n    Path(\"/a]b\"),\n    Path(\"/home/u/ProtonDrive\"),\n]");
        assert_eq!(favorites_sync(Some(&added), true, None, &mount()).unwrap().unwrap(), added);
        assert_eq!(favorites_sync(Some(&added), false, None, &mount()).unwrap().unwrap(), FAVORITES);
    }

    #[test]
    fn favorite_without_trailing_comma_or_newline_and_empty_list() {
        let added = favorites_sync(Some("[Home]"), true, None, &mount()).unwrap().unwrap();
        assert_eq!(added, "[Home,\n    Path(\"/home/u/ProtonDrive\"),\n]");
        let added = favorites_sync(Some("[\n]\n"), true, None, &mount()).unwrap().unwrap();
        assert_eq!(added, "[\n    Path(\"/home/u/ProtonDrive\"),\n]\n");
        assert!(favorites_sync(Some("garbage"), true, None, &mount()).is_err());
    }

    #[test]
    fn favorite_follows_a_new_mount_point() {
        let added = favorites_sync(Some(FAVORITES), true, None, &mount()).unwrap().unwrap();
        let moved = favorites_sync(Some(&added), true, Some(&mount()), Path::new("/mnt/pd")).unwrap().unwrap();
        assert!(!moved.contains("ProtonDrive"));
        assert!(moved.contains("Path(\"/mnt/pd\"),\n]"));
        assert!(moved.contains("Path(\"/mnt/data\")"));
    }

    #[test]
    fn missing_favorites_file_gets_the_defaults() {
        let text = favorites_sync(None, true, None, &mount()).unwrap().unwrap();
        assert_eq!(text, "[\n    Home,\n    Documents,\n    Downloads,\n    Music,\n    Pictures,\n    Videos,\n    Path(\"/home/u/ProtonDrive\"),\n]");
        assert_eq!(favorites_sync(None, false, None, &mount()).unwrap(), None);
    }

    // context actions

    const MINE: &str = "[\n    (name: \"Mine\", confirm: false, selection: Files, steps: [\"mytool %F\"]),\n]";

    #[test]
    fn actions_are_appended_after_the_users_own() {
        let added = context_actions_sync(Some(MINE), true, &actions()).unwrap().unwrap();
        assert!(added.starts_with(&MINE[..MINE.len() - 2]));
        assert!(added.contains("    (name: \"PDrive: Keep Available Offline\", confirm: false, selection: Any, steps: [\"pdrive keep-offline %F\"]),\n"));
        assert!(added.contains("    (name: \"PDrive: Free Up Space\", confirm: false, selection: Any, steps: [\"pdrive free-up-space %F\"]),\n]"));
        assert_eq!(context_actions_sync(Some(&added), true, &actions()).unwrap().unwrap(), added);
        assert_eq!(context_actions_sync(Some(&added), false, &actions()).unwrap().unwrap(), MINE);
    }

    #[test]
    fn actions_follow_a_language_or_binary_change() {
        let added = context_actions_sync(Some(MINE), true, &actions()).unwrap().unwrap();
        let french = Actions {
            keep_name: "PDrive : garder disponible hors ligne".into(),
            free_name: "PDrive : libérer de l'espace".into(),
            command: "/home/u/my bin/pdrive".into(),
        };
        let changed = context_actions_sync(Some(&added), true, &french).unwrap().unwrap();
        assert!(changed.contains("name: \"PDrive : garder disponible hors ligne\""));
        assert!(changed.contains("steps: [\"\\\"/home/u/my bin/pdrive\\\" free-up-space %F\"]"));
        assert!(changed.contains("\"mytool %F\""));
        assert_eq!(changed.matches("pdrive").count(), 2);
        // Still recognised, so removable.
        assert_eq!(context_actions_sync(Some(&changed), false, &french).unwrap().unwrap(), MINE);
    }

    #[test]
    fn duplicates_are_merged_and_missing_files_created() {
        let entry = actions().entry(Subcommand::KeepOffline);
        let doubled = format!("[\n    {entry},\n    {entry},\n]");
        let fixed = context_actions_sync(Some(&doubled), true, &actions()).unwrap().unwrap();
        assert_eq!(fixed.matches("keep-offline").count(), 1);
        assert_eq!(fixed.matches("free-up-space").count(), 1);
        let created = context_actions_sync(None, true, &actions()).unwrap().unwrap();
        assert_eq!(created.matches("(name:").count(), 2);
        assert_eq!(context_actions_sync(Some(&created), false, &actions()).unwrap().unwrap(), "[\n]");
        assert_eq!(context_actions_sync(None, false, &actions()).unwrap(), None);
    }

    #[test]
    fn comments_and_nested_parentheses_do_not_confuse_the_scan() {
        let text = "[\n    // (not an element)\n    (name: \"a)b\", confirm: true, selection: Any, steps: [\"x (y\"]),\n]";
        let added = context_actions_sync(Some(text), true, &actions()).unwrap().unwrap();
        assert!(added.starts_with(&text[..text.len() - 2]));
        assert_eq!(context_actions_sync(Some(&added), false, &actions()).unwrap().unwrap(), text);
    }

    // Files

    struct Fixture {
        _dir: tempfile::TempDir,
        env: Env,
    }

    fn fixture(cosmic_installed: bool) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let (data_home, system, config_home) = (dir.path().join("data"), dir.path().join("system"), dir.path().join("config"));
        std::fs::create_dir_all(system.join("applications")).unwrap();
        std::fs::create_dir_all(&data_home).unwrap();
        if cosmic_installed {
            std::fs::write(system.join("applications").join(COSMIC_FILES_DESKTOP), "[Desktop Entry]\n").unwrap();
        }
        Fixture { env: Env { data_home, data_dirs: vec![system], config_home }, _dir: dir }
    }

    #[test]
    fn nothing_is_created_for_file_managers_that_are_not_there() {
        let f = fixture(false);
        assert!(apply(&f.env, true, None, &mount(), &actions()).is_empty());
        assert!(!f.env.xbel().exists());
        assert!(!f.env.cosmic_dir().exists());
        assert!(apply(&f.env, false, None, &mount(), &actions()).is_empty());
    }

    #[test]
    fn both_managers_are_set_up_and_removed_whatever_the_session() {
        let f = fixture(true);
        std::fs::write(f.env.xbel(), XBEL).unwrap();
        // The session is irrelevant: it is not even read.
        assert!(apply(&f.env, true, None, &mount(), &actions()).is_empty());
        assert!(std::fs::read_to_string(f.env.xbel()).unwrap().contains(XBEL_MARKER));
        let favorites = std::fs::read_to_string(f.env.cosmic_dir().join("favorites")).unwrap();
        assert!(favorites.contains("Videos,\n    Path(\"/home/u/ProtonDrive\"),\n]"));
        let context = std::fs::read_to_string(f.env.cosmic_dir().join("context_actions")).unwrap();
        assert_eq!(context.matches("selection: Any").count(), 2);

        // A second pass writes nothing.
        let before = std::fs::metadata(f.env.xbel()).unwrap().modified().unwrap();
        assert!(apply(&f.env, true, None, &mount(), &actions()).is_empty());
        assert_eq!(std::fs::metadata(f.env.xbel()).unwrap().modified().unwrap(), before);

        assert!(apply(&f.env, false, None, &mount(), &actions()).is_empty());
        assert_eq!(std::fs::read_to_string(f.env.xbel()).unwrap(), XBEL);
        assert!(!std::fs::read_to_string(f.env.cosmic_dir().join("favorites")).unwrap().contains("ProtonDrive"));
        assert!(!std::fs::read_to_string(f.env.cosmic_dir().join("context_actions")).unwrap().contains("pdrive"));
    }

    #[test]
    fn existing_cosmic_files_are_edited_in_place_and_links_are_kept() {
        let f = fixture(true);
        std::fs::create_dir_all(f.env.cosmic_dir()).unwrap();
        let real = f.env.config_home.join("dotfiles-favorites");
        std::fs::write(&real, FAVORITES).unwrap();
        let link = f.env.cosmic_dir().join("favorites");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(apply(&f.env, true, None, &mount(), &actions()).is_empty());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        assert!(std::fs::read_to_string(&real).unwrap().contains("Path(\"/home/u/ProtonDrive\")"));
        assert!(std::fs::read_to_string(&real).unwrap().contains("Path(\"/mnt/data\")"));
        // No leftover temporary file.
        assert_eq!(std::fs::read_dir(f.env.config_home.as_path()).unwrap().count(), 2);
    }

    #[test]
    fn an_unreadable_file_does_not_stop_the_other_manager() {
        let f = fixture(true);
        std::fs::create_dir_all(f.env.cosmic_dir()).unwrap();
        std::fs::write(f.env.cosmic_dir().join("favorites"), "garbage").unwrap();
        std::fs::write(f.env.xbel(), XBEL).unwrap();
        let errors = apply(&f.env, true, None, &mount(), &actions());
        assert_eq!(errors.len(), 1);
        assert_eq!(std::fs::read_to_string(f.env.cosmic_dir().join("favorites")).unwrap(), "garbage");
        assert!(std::fs::read_to_string(f.env.xbel()).unwrap().contains(XBEL_MARKER));
    }

    #[test]
    fn a_changed_mount_point_moves_both_entries() {
        let f = fixture(true);
        std::fs::write(f.env.xbel(), XBEL).unwrap();
        apply(&f.env, true, None, &mount(), &actions());
        let new = PathBuf::from("/mnt/pd");
        assert!(apply(&f.env, true, Some(&mount()), &new, &actions()).is_empty());
        let xbel = std::fs::read_to_string(f.env.xbel()).unwrap();
        assert!(xbel.contains("href=\"file:///mnt/pd\"") && !xbel.contains("ProtonDrive"));
        let favorites = std::fs::read_to_string(f.env.cosmic_dir().join("favorites")).unwrap();
        assert!(favorites.contains("Path(\"/mnt/pd\")") && !favorites.contains("ProtonDrive"));
    }
}
