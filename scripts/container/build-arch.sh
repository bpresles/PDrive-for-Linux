#!/usr/bin/env bash
# Runs inside an Arch Linux container (local script and GitHub workflow). /dist holds the two
# sources and receives the .pkg.tar.zst. makepkg refuses to run as root: the build is done by
# an unprivileged user.
set -euo pipefail
pacman -Syu --noconfirm --needed base-devel cargo gettext desktop-file-utils pkgconf nautilus gtk4 libadwaita \
    glib2 fuse3 xz tar namcap git
useradd --create-home builder
B=/home/builder/build; mkdir -p "$B"
cp "/dist/pdrive-for-linux-$VERSION.tar.xz" "/dist/$NATIVE_ARCHIVE" "$B/"
# PKGBUILD taken from the source tarball; the native archive is named after its identity there.
tar -xJf "/dist/pdrive-for-linux-$VERSION.tar.xz" -C /tmp "pdrive-for-linux-$VERSION/packaging/arch/PKGBUILD"
cp "/tmp/pdrive-for-linux-$VERSION/packaging/arch/PKGBUILD" "$B/"
chown -R builder: "$B"
su builder -c "cd '$B' && makepkg --force --noconfirm"
cp "$B"/*.pkg.tar.zst /dist/
namcap "$B"/*.pkg.tar.zst || true
