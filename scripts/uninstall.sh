#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Bertrand Presles
# Uninstalls what scripts/install.sh and scripts/install-system.sh installed (the latter with
# sudo), including the files of earlier versions (systemd service, former application id).
# Stops PDrive first if it runs from ~/.local/bin: the drive is unmounted and uploads still
# pending are resumed at the next start of any installation.
# Kept: the session in the keyring, the settings (~/.config/pdrive), the cache and pending
# uploads (~/.cache/pdrive), the offline choices (~/.local/share/pdrive) and the ~/ProtonDrive
# link, which a package installation reuses; see the end of this script to remove them.
# Files installed by an RPM or DEB package are left alone.
set -euo pipefail

BIN_DIR="$HOME/.local/bin"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
UNIT_DIR="$CONFIG_HOME/systemd/user"
ICON_ROOT="$DATA_HOME/icons/hicolor"
STOP_TIMEOUT=30

# Processes of this installation, not of a package one.
local_pids() {
    local pid
    for pid in $(pgrep -x "$1" || true); do
        if [[ "$(readlink "/proc/$pid/exe" 2>/dev/null)" == "$BIN_DIR/$1" ]]; then
            echo "$pid"
        fi
    done
}

# Stop the application (it stops the daemon it runs), then a daemon started alone.
for program in pdrive-app pdrived; do
    pids=$(local_pids "$program")
    [[ -z "$pids" ]] && continue
    echo "Stopping $program..."
    kill -TERM $pids 2>/dev/null || true
    for _ in $(seq "$STOP_TIMEOUT"); do
        [[ -z "$(local_pids pdrive-app)$(local_pids pdrived)" ]] && break
        sleep 1
    done
done
if [[ -n "$(local_pids pdrive-app)$(local_pids pdrived)" ]]; then
    echo "PDrive is still running: quit it from the tray menu, then run this script again." >&2
    exit 1
fi

# Systemd services of earlier versions.
for unit in pdrive.service nautilus-drive-poc.service; do
    if [[ -f "$UNIT_DIR/$unit" ]]; then
        systemctl --user disable --now "$unit" || true
        rm -f "$UNIT_DIR/$unit"
        UNITS_REMOVED=1
    fi
done
[[ -n "${UNITS_REMOVED:-}" ]] && systemctl --user daemon-reload

# User installation (install.sh).
rm -f "$BIN_DIR"/{pdrive-app,pdrived,pdrive,pdfs,pdpoc}
rm -rf "$HOME/.local/lib/pdrive" "$HOME/.local/lib/nautilus-drive-poc"
for app_id in io.github.bpresles.PDrive.App io.github.pdrive.PDrive.App; do
    rm -f "$DATA_HOME/applications/$app_id.desktop" "$CONFIG_HOME/autostart/$app_id.desktop"
done
if [[ -d "$ICON_ROOT" ]]; then
    find "$ICON_ROOT" \( -name 'io.github.*PDrive.App.svg' -o -name 'emblem-pdrive-*.svg' -o -name 'pdrive-*.svg' \) -delete
    gtk-update-icon-cache -q -t "$ICON_ROOT" 2>/dev/null || true
fi
rm -f "$DATA_HOME"/locale/*/LC_MESSAGES/pdrive.mo
rm -f "$DATA_HOME"/man/man1/{pdrive,pdrived,pdrive-app}.1
update-desktop-database -q "$DATA_HOME/applications" 2>/dev/null || true

# System installation (install-system.sh): the Nautilus extension, unless a package owns it.
extension_dirs=("$(pkg-config --variable=extensiondir libnautilus-extension-4 2>/dev/null || true)" /usr/lib64/nautilus/extensions-4 /usr/lib/*/nautilus/extensions-4)
system_files=()
for dir in "${extension_dirs[@]}"; do
    file="$dir/libpdrive-nautilus.so"
    [[ -n "$dir" && -f "$file" ]] || continue
    if rpm -qf "$file" >/dev/null 2>&1 || dpkg -S "$file" >/dev/null 2>&1; then
        echo "Kept $file: it belongs to an installed package."
        continue
    fi
    [[ " ${system_files[*]} " == *" $file "* ]] || system_files+=("$file")
done
for file in /usr/local/share/cloud-providers/io.github.{bpresles,pdrive}.PDrive.ini /usr/local/share/locale/*/LC_MESSAGES/pdrive.mo; do
    [[ -f "$file" ]] && system_files+=("$file")
done
if (( ${#system_files[@]} )); then
    echo "Removing system files (sudo): ${system_files[*]}"
    sudo rm -f "${system_files[@]}"
fi

cat <<EOF

Uninstalled. Restart Nautilus to unload the extension: nautilus -q

Your data was kept. To remove it as well:
  pdrive logout          # before uninstalling, or with a package installation: revokes the session
  rm -rf ${CONFIG_HOME}/pdrive ${XDG_CACHE_HOME:-$HOME/.cache}/pdrive ${DATA_HOME}/pdrive   # loses pending uploads
  rm -f ~/ProtonDrive    # the link to the mount
Files stored in Proton Drive are not affected.
EOF
