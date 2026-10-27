#!/usr/bin/env bash
# Installs PDrive for Linux for the current user:
#   ~/.local/lib/pdrive/                    native libraries (Proton Drive SDK and crypto)
#   ~/.local/bin/pdrived, pdrive            mount daemon and command-line tool (login, logout)
#   ~/.config/systemd/user/pdrive.service
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
LIB_DIR="$HOME/.local/lib/pdrive"
BIN_DIR="$HOME/.local/bin"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
UNIT="pdrive.service"

if [[ ! -f "$ROOT/native/linux-x64/libproton_drive_sdk.so" ]]; then
    "$ROOT/scripts/build-native.sh"
fi

mkdir -p "$LIB_DIR" "$BIN_DIR" "$UNIT_DIR"
install -m 0644 "$ROOT/native/linux-x64/"lib{proton_drive_sdk,proton_crypto}.so "$LIB_DIR/"

# The binaries find the libraries through an rpath to the installed directory.
PDSDK_LIB_DIR="$LIB_DIR" cargo build --release --manifest-path "$ROOT/Cargo.toml" -p pdrived -p pdrive
install -m 0755 "$ROOT/target/release/pdrived" "$ROOT/target/release/pdrive" "$BIN_DIR/"

# Remove the installation made before the project got its name.
LEGACY_UNIT="nautilus-drive-poc.service"
if [[ -f "$UNIT_DIR/$LEGACY_UNIT" ]]; then
    systemctl --user disable --now "$LEGACY_UNIT" || true
    rm -f "$UNIT_DIR/$LEGACY_UNIT" "$BIN_DIR/pdfs" "$BIN_DIR/pdpoc"
    rm -rf "$HOME/.local/lib/nautilus-drive-poc"
    LEGACY_REMOVED=1
fi

install -m 0644 "$ROOT/packaging/$UNIT" "$UNIT_DIR/$UNIT"
systemctl --user daemon-reload

if [[ -n "${LEGACY_REMOVED:-}" ]]; then
    echo "The previous installation (nautilus-drive-poc) was removed; enabling $UNIT instead."
    systemctl --user enable --now "$UNIT"
fi

cat <<EOF

Installed. Next steps:
  pdrive login                                  # once: sign in through the browser
  systemctl --user enable --now $UNIT           # mount now and at each login
  journalctl --user -u $UNIT -f                 # follow the logs

The mount point and the cache size are set in
${XDG_CONFIG_HOME:-$HOME/.config}/pdrive/config.toml (default: ~/ProtonDrive, 10 GiB).
EOF
