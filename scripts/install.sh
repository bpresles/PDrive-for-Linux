#!/usr/bin/env bash
# Installs the client for the current user:
#   ~/.local/lib/nautilus-drive-poc/   native libraries (Proton Drive SDK and crypto)
#   ~/.local/bin/pdfs, pdpoc           mount daemon and command-line tool (login, logout)
#   ~/.config/systemd/user/nautilus-drive-poc.service
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB_DIR="$HOME/.local/lib/nautilus-drive-poc"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT="nautilus-drive-poc.service"

if [[ ! -f "$ROOT/native/linux-x64/libproton_drive_sdk.so" ]]; then
    "$ROOT/scripts/build-native.sh"
fi

mkdir -p "$LIB_DIR" "$BIN_DIR" "$UNIT_DIR"
install -m 0644 "$ROOT/native/linux-x64/"lib{proton_drive_sdk,proton_crypto}.so "$LIB_DIR/"

# The binaries find the libraries through an rpath to the installed directory.
PDSDK_LIB_DIR="$LIB_DIR" cargo build --release --manifest-path "$ROOT/Cargo.toml" -p pdfs -p pdpoc
install -m 0755 "$ROOT/target/release/pdfs" "$ROOT/target/release/pdpoc" "$BIN_DIR/"

install -m 0644 "$ROOT/packaging/$UNIT" "$UNIT_DIR/$UNIT"
systemctl --user daemon-reload

cat <<EOF

Installed. Next steps:
  pdpoc login                                   # once: sign in through the browser
  systemctl --user enable --now $UNIT           # mount now and at each login
  journalctl --user -u $UNIT -f                 # follow the logs

The mount point and the cache size are set in
${XDG_CONFIG_HOME:-$HOME/.config}/nautilus-drive-poc/config.toml (default: ~/ProtonDrive, 10 GiB).
EOF
