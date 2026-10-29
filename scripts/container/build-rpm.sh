#!/usr/bin/env bash
# Runs inside a Fedora container (local script and GitHub workflow). /dist holds the two
# sources and receives the RPM and SRPM. Spec taken from the source tarball.
set -euo pipefail
dnf -y install rpm-build rpmlint cargo rust gcc make binutils gettext desktop-file-utils \
    'pkgconfig(libnautilus-extension-4)' 'pkgconfig(gtk4)' 'pkgconfig(libadwaita-1)' \
    'pkgconfig(glib-2.0)' 'pkgconfig(gio-2.0)' 'pkgconfig(gmodule-2.0)' xz tar
TOP=/tmp/rpmbuild; mkdir -p "$TOP"/{SOURCES,SPECS}
cp "/dist/pdrive-for-linux-$VERSION.tar.xz" "/dist/$NATIVE_ARCHIVE" "$TOP/SOURCES/"
tar -xJf "/dist/pdrive-for-linux-$VERSION.tar.xz" -C /tmp "pdrive-for-linux-$VERSION/packaging/rpm/pdrive-for-linux.spec"
cp "/tmp/pdrive-for-linux-$VERSION/packaging/rpm/pdrive-for-linux.spec" "$TOP/SPECS/"
# Sources are local files: the spec URLs only name them.
rpmbuild -ba --define "_topdir $TOP" "$TOP/SPECS/pdrive-for-linux.spec"
cp "$TOP"/RPMS/*/*.rpm "$TOP"/SRPMS/*.src.rpm /dist/
rpmlint "$TOP"/RPMS/*/*.rpm "$TOP"/SRPMS/*.src.rpm || true
