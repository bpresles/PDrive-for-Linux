#!/usr/bin/env bash
# Installs the parts of PDrive for Linux that must live in system directories (needs sudo):
#   the Nautilus extension (Nautilus only loads extensions from its system directory), and
#   the libcloudproviders declaration (only read from system data directories),
#   and the translations of the extension, in the LOCALEDIR it is built with.
# Requires nautilus-devel to build the extension.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOUD_PROVIDERS_DIR=/usr/local/share/cloud-providers
LOCALEDIR=/usr/local/share/locale

make -C "$ROOT/extension" LOCALEDIR="$LOCALEDIR"
sudo make -C "$ROOT/extension" LOCALEDIR="$LOCALEDIR" install
sudo install -Dm0644 "$ROOT/packaging/io.github.pdrive.PDrive.ini" "$CLOUD_PROVIDERS_DIR/io.github.pdrive.PDrive.ini"

while read -r lang; do
    [[ -z "$lang" || "$lang" == \#* ]] && continue
    msgfmt --check -o "$ROOT/po/$lang.mo" "$ROOT/po/$lang.po"
    sudo install -Dm0644 "$ROOT/po/$lang.mo" "$LOCALEDIR/$lang/LC_MESSAGES/pdrive.mo"
    rm -f "$ROOT/po/$lang.mo"
done < "$ROOT/po/LINGUAS"

echo "Installed. Restart Nautilus to load the extension: nautilus -q"
