#!/usr/bin/env bash
# Installs PDrive for Linux for the current user:
#   ~/.local/lib/pdrive/                    native libraries (Proton Drive SDK and crypto)
#   ~/.local/bin/pdrive-app, pdrived, pdrive  desktop application (tray icon, status window; it
#                                           runs pdrived), mount daemon, command-line tool
#   ~/.local/share/applications/io.github.bpresles.PDrive.App.desktop
#   ~/.local/share/icons/hicolor/scalable/   application and tray icons, sync state emblems
#                                           for Nautilus
#   ~/.local/share/locale/<lang>/LC_MESSAGES/pdrive.mo   translations (po/), found by the programs
#                                           through the prefix of their executable
# It also removes the systemd user service of earlier versions (pdrive.service).
# The Nautilus extension and the sidebar declaration are installed by install-system.sh (sudo).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB_DIR="$HOME/.local/lib/pdrive"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
UNIT="pdrive.service"

if [[ ! -f "$ROOT/native/linux-x64/libproton_drive_sdk.so" ]]; then
    "$ROOT/scripts/build-native.sh"
fi

mkdir -p "$LIB_DIR" "$BIN_DIR" "$UNIT_DIR" "$APP_DIR"
install -m 0644 "$ROOT/native/linux-x64/"lib{proton_drive_sdk,proton_crypto}.so "$LIB_DIR/"

# The binaries find the libraries through an rpath to the installed directory.
PDSDK_LIB_DIR="$LIB_DIR" cargo build --release --manifest-path "$ROOT/Cargo.toml" -p pdrived -p pdrive -p pdrive-app
install -m 0755 "$ROOT/target/release/pdrived" "$ROOT/target/release/pdrive" "$ROOT/target/release/pdrive-app" "$BIN_DIR/"

# Remove the installation made before the project got its name.
LEGACY_UNIT="nautilus-drive-poc.service"
if [[ -f "$UNIT_DIR/$LEGACY_UNIT" ]]; then
    systemctl --user disable --now "$LEGACY_UNIT" || true
    rm -f "$UNIT_DIR/$LEGACY_UNIT" "$BIN_DIR/pdfs" "$BIN_DIR/pdpoc"
    rm -rf "$HOME/.local/lib/nautilus-drive-poc"
    LEGACY_REMOVED=1
fi

# Migrate from the systemd service: the application now runs the daemon.
if [[ -f "$UNIT_DIR/$UNIT" ]]; then
    systemctl --user disable --now "$UNIT" || true
    rm -f "$UNIT_DIR/$UNIT"
    systemctl --user daemon-reload
    SERVICE_REMOVED=1
fi

ICON_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
# Migrate from the former application id (io.github.pdrive.PDrive.App).
rm -f "$APP_DIR/io.github.pdrive.PDrive.App.desktop" "$ICON_ROOT/scalable/apps/io.github.pdrive.PDrive.App.svg"
(cd "$ROOT/packaging/icons/hicolor" && find . -name '*.svg' -print0 | while IFS= read -r -d '' icon; do
    install -Dm 0644 "$icon" "$ICON_ROOT/$icon"
done)
gtk-update-icon-cache -q -t "$ICON_ROOT" 2>/dev/null || true

# Translations: one catalogue per language of po/LINGUAS.
LOCALE_ROOT="${XDG_DATA_HOME:-$HOME/.local/share}/locale"
while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    mkdir -p "$LOCALE_ROOT/$lang/LC_MESSAGES"
    msgfmt --check -o "$LOCALE_ROOT/$lang/LC_MESSAGES/pdrive.mo" "$ROOT/po/$lang.po"
done < "$ROOT/po/LINGUAS"

# The Exec line gets the absolute path: ~/.local/bin is not always in the PATH of the menus.
sed "s|^Exec=pdrive-app|Exec=$BIN_DIR/pdrive-app|" "$ROOT/packaging/io.github.bpresles.PDrive.App.desktop" > "$APP_DIR/io.github.bpresles.PDrive.App.desktop"
update-desktop-database -q "$APP_DIR" 2>/dev/null || true

if [[ -n "${LEGACY_REMOVED:-}" || -n "${SERVICE_REMOVED:-}" ]]; then
    echo "The previous systemd service was removed: PDrive is now started by pdrive-app."
    echo "If pdrived is still running, stop it (pkill pdrived) before starting pdrive-app."
fi

cat <<EOF

Installed. Next steps:
  pdrive-app                                    # or "PDrive" in the applications menu: runs the
                                                # daemon, shows the tray icon and the status window
  Sign in from the window ("Sign in"), or with: pdrive login
  Mount point, cache, start at login and sign out: settings of the window
  scripts/install-system.sh                     # Nautilus emblems, menu and sidebar (sudo)

Logs: from a terminal, pdrive-app (and the daemon it runs) logs on its standard error output.
Started from the menu or at login, they go to the journal of your session:
  journalctl --user -t pdrive -f
or, if there is no journal, to ${XDG_STATE_HOME:-$HOME/.local/state}/pdrive/pdrive.log.
On GNOME, the tray icon needs the "AppIndicator and KStatusNotifierItem Support" extension.

The mount point and the cache size are set in
${XDG_CONFIG_HOME:-$HOME/.config}/pdrive/config.toml (default: ~/ProtonDrive, 10 GiB).
EOF
