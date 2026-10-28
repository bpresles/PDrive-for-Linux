#!/usr/bin/env bash
# Runs inside a Debian container (local script and GitHub workflow). /dist holds the two
# sources and receives the .deb. Toolchain: rustc/cargo of the distribution when recent
# enough (>= 1.92), else rustup (RUSTUP_TOOLCHAIN, default stable) with build-deps unchecked.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends build-essential debhelper dpkg-dev fakeroot patchelf gettext \
    desktop-file-utils pkg-config libnautilus-extension-dev libgtk-4-dev libadwaita-1-dev libglib2.0-dev \
    xz-utils lintian ca-certificates curl cargo rustc
DPKG_FLAGS=()
if ! dpkg --compare-versions "$(rustc --version | awk '{print $2}')" ge 1.92; then
    : "${RUSTUP_TOOLCHAIN:=stable}"
    apt-get remove -y cargo rustc
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain "$RUSTUP_TOOLCHAIN"
    export PATH="$HOME/.cargo/bin:$PATH"
    DPKG_FLAGS=(-d)
fi
rustc --version
W=/tmp/build; rm -rf "$W"; mkdir -p "$W"
tar -xJf "/dist/pdrive-for-linux-$VERSION.tar.xz" -C "$W"
S="$W/pdrive-for-linux-$VERSION"
tar -xJf "/dist/$NATIVE_ARCHIVE" -C "$W"
mkdir -p "$S/native/linux-x64"
cp "$W"/pdrive-native-linux-x64-*/* "$S/native/linux-x64/"
cd "$S"
dpkg-buildpackage -b -uc -us "${DPKG_FLAGS[@]}"
cp "$W"/*.deb /dist/
lintian --info "$W"/*.deb || true
