#!/usr/bin/env bash
# Installs the parts of PDrive for Linux that must live in system directories (needs sudo):
#   the Nautilus extension (Nautilus only loads extensions from its system directory), and
#   the libcloudproviders declaration (only read from system data directories).
# Requires nautilus-devel to build the extension.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLOUD_PROVIDERS_DIR=/usr/local/share/cloud-providers

make -C "$ROOT/extension"
sudo make -C "$ROOT/extension" install
sudo install -Dm0644 "$ROOT/packaging/io.github.pdrive.PDrive.ini" "$CLOUD_PROVIDERS_DIR/io.github.pdrive.PDrive.ini"

echo "Installed. Restart Nautilus to load the extension: nautilus -q"
