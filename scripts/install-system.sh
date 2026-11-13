#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Bertrand Presles
# Installs the parts of PDrive for Linux that must live in system directories (needs sudo):
#   the Nautilus extension (Nautilus only loads extensions from its system directory),
#   the Dolphin plugins (KDE Frameworks 6: sync state emblems and menu entries, loaded from the
#   Qt plugin directory of the system),
#   the libcloudproviders declaration (only read from system data directories),
#   and the translations of the plugins, in the LOCALEDIR the extension is built with.
# Each file manager is handled if its development files are found, and skipped otherwise:
#   Nautilus needs nautilus-devel (pkg-config libnautilus-extension-4),
#   Dolphin needs cmake, extra-cmake-modules, kf6-kio-devel, kf6-kcoreaddons-devel,
#   kf6-ki18n-devel and qt6-qtbase-devel (Debian: libkf6kio-dev, libkf6coreaddons-dev,
#   libkf6i18n-dev, qt6-base-dev).
# COSMIC Files needs nothing here: pdrive-app sets it up in the configuration of the user.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOUD_PROVIDERS_DIR=/usr/local/share/cloud-providers
LOCALEDIR=/usr/local/share/locale
restart_hints=()

if pkg-config --exists libnautilus-extension-4; then
    make -C "$ROOT/extension" LOCALEDIR="$LOCALEDIR"
    sudo make -C "$ROOT/extension" LOCALEDIR="$LOCALEDIR" install
    restart_hints+=("Nautilus: nautilus -q")
else
    echo "Nautilus extension skipped: libnautilus-extension-4 not found (install nautilus-devel)."
fi

dolphin_build="$ROOT/integrations/dolphin/build"
qt_plugins="$( (qtpaths6 --query QT_INSTALL_PLUGINS || qmake6 -query QT_INSTALL_PLUGINS) 2>/dev/null | head -n1 || true)"
if [[ -z "$qt_plugins" ]]; then
    echo "Dolphin plugins skipped: qtpaths6 not found (install qt6-qtbase-devel)."
elif cmake -S "$ROOT/integrations/dolphin" -B "$dolphin_build" -DCMAKE_BUILD_TYPE=Release >/dev/null 2>&1; then
    cmake --build "$dolphin_build"
    sudo install -Dm0755 "$dolphin_build/bin/kf6/overlayicon/pdriveoverlay.so" "$qt_plugins/kf6/overlayicon/pdriveoverlay.so"
    sudo install -Dm0755 "$dolphin_build/bin/kf6/kfileitemaction/pdriveactions.so" "$qt_plugins/kf6/kfileitemaction/pdriveactions.so"
    restart_hints+=("Dolphin: close all its windows, then start it again")
else
    echo "Dolphin plugins skipped: CMake did not find KF6 KIO (see the requirements above)."
fi
rm -rf "$dolphin_build"

# Migrate from the former D-Bus name (io.github.pdrive.PDrive).
sudo rm -f "$CLOUD_PROVIDERS_DIR/io.github.pdrive.PDrive.ini"
sudo install -Dm0644 "$ROOT/packaging/io.github.bpresles.PDrive.ini" "$CLOUD_PROVIDERS_DIR/io.github.bpresles.PDrive.ini"

while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    msgfmt --check -o "$ROOT/po/$lang.mo" "$ROOT/po/$lang.po"
    sudo install -Dm0644 "$ROOT/po/$lang.mo" "$LOCALEDIR/$lang/LC_MESSAGES/pdrive.mo"
    rm -f "$ROOT/po/$lang.mo"
done < "$ROOT/po/LINGUAS"

if (( ${#restart_hints[@]} )); then
    echo "Installed. Restart the file managers to load the plugins:"
    printf '  %s\n' "${restart_hints[@]}"
else
    echo "Installed (no file manager plugin: none of the development packages above was found)."
fi
