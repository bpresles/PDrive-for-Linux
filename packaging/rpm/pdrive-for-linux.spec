# PDrive for Linux: unofficial Proton Drive client (not affiliated with Proton AG).
#
# Build without network: Source0 holds the project and its vendored Rust crates
# (scripts/make-source-tarball.sh), Source1 the prebuilt native libraries
# (scripts/package-native.sh). Not intended for the official Fedora repositories (bundled
# prebuilt binaries and vendored crates); built for COPR and GitHub releases.

# Revisions of the pinned Proton sources the native archive was built from.
# Kept in sync with scripts/build-native.sh by scripts/check-version.sh.
%global native_id b68d64b-fc2c108

# Names tied to the D-Bus / application identifiers (PLAN.md renames them together).
%global app_id io.github.bpresles.PDrive.App
%global cloud_ini io.github.bpresles.PDrive.ini

# Private libraries: neither provided to nor required from the rest of the system.
%global __provides_exclude_from ^%{_libdir}/pdrive/.*$
%global __requires_exclude ^libproton_(drive_sdk|crypto)\\.so.*$

Name:           pdrive-for-linux
Version:        0.1.0
Release:        1%{?dist}
Summary:        Unofficial Proton Drive client for Linux (FUSE mount, Nautilus integration)
# GPL-3.0-or-later: this project. Bundled components, audited 29/10/2026 (THIRD_PARTY_NOTICES,
# licenses/*.tsv): MIT (Proton SDK and cryptography, .NET runtime and Microsoft NuGet packages,
# most Rust crates), Apache-2.0, BSD-3-Clause, ISC, Zlib, Unicode-3.0, Unlicense, CDLA-Permissive-2.0
# (webpki-roots), bzip2-1.0.6 (libbz2-rs-sys), SQLite public domain (SQLite package).
License:        GPL-3.0-or-later AND MIT AND Apache-2.0 AND BSD-3-Clause AND ISC AND Zlib AND Unicode-3.0 AND Unlicense AND CDLA-Permissive-2.0 AND bzip2-1.0.6
URL:            https://github.com/bpresles/PDrive-for-Linux
Source0:        %{url}/releases/download/v%{version}/%{name}-%{version}.tar.xz
Source1:        %{url}/releases/download/v%{version}/pdrive-native-linux-x64-%{native_id}.tar.xz

# The native libraries are prebuilt for x86_64 only.
ExclusiveArch:  x86_64

BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  cargo >= 1.92
BuildRequires:  rust >= 1.92
BuildRequires:  binutils
BuildRequires:  gettext
BuildRequires:  desktop-file-utils
BuildRequires:  pkgconfig(libnautilus-extension-4)
BuildRequires:  pkgconfig(gtk4) >= 4.12
BuildRequires:  pkgconfig(libadwaita-1) >= 1.6
BuildRequires:  pkgconfig(glib-2.0)
BuildRequires:  pkgconfig(gio-2.0)
BuildRequires:  pkgconfig(gmodule-2.0)

# fuser is built without libfuse: the daemon calls fusermount3.
Requires:       fuse3
# Tokens are stored through the Secret Service API.
Requires:       (gnome-keyring or kf6-kwallet or keepassxc)
Requires:       hicolor-icon-theme
Recommends:     gnome-shell-extension-appindicator
Recommends:     nautilus

%description
PDrive for Linux mounts Proton Drive as a FUSE file system, with on-demand
download, offline pinning, sync state emblems and menu entries in Nautilus, a
desktop application (tray icon, status window) that runs the daemon, and a
command-line tool.

This is an unofficial third-party client, not affiliated with or endorsed by
Proton AG. It bundles the Proton Drive SDK and Proton cryptography libraries
(MIT) as private libraries.

%prep
%setup -q
# Native libraries, verified against the sums shipped in their archive.
tar -xf %{SOURCE1}
( cd pdrive-native-linux-x64-%{native_id} && sha256sum -c SHA256SUMS )
mkdir -p native/linux-x64
install -m0755 pdrive-native-linux-x64-%{native_id}/*.so native/linux-x64/
# One version everywhere: the tarball must carry the version of the spec.
cargo_version=$(awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version *=/{gsub(/"/,"",$3); print $3; exit}' Cargo.toml)
test "$cargo_version" = "%{version}" || { echo "Cargo.toml version $cargo_version != spec %{version}" >&2; exit 1; }

%build
export PDSDK_LIB_DIR="$PWD/native/linux-x64"
# Run-time location (rpath, and where the SDK loads libproton_crypto.so from).
export PDSDK_RUNTIME_LIB_DIR=%{_libdir}/pdrive
export RUSTFLAGS="%{build_rustflags}"
cargo build --release --frozen --offline -p pdrived -p pdrive -p pdrive-app

# Nautilus extension. Environment CFLAGS are appended to the Makefile ones, LDFLAGS are used at link time.
%set_build_flags
make -C extension LOCALEDIR=%{_datadir}/locale

%install
install -Dpm0755 target/release/pdrive target/release/pdrived target/release/pdrive-app -t %{buildroot}%{_bindir}
install -Dpm0755 native/linux-x64/libproton_drive_sdk.so native/linux-x64/libproton_crypto.so -t %{buildroot}%{_libdir}/pdrive

make -C extension install DESTDIR=%{buildroot} LOCALEDIR=%{_datadir}/locale EXTENSION_DIR=%{_libdir}/nautilus/extensions-4

install -Dpm0644 packaging/man/*.1 -t %{buildroot}%{_mandir}/man1
install -Dpm0644 packaging/%{cloud_ini} %{buildroot}%{_datadir}/cloud-providers/%{cloud_ini}
desktop-file-install --dir=%{buildroot}%{_datadir}/applications packaging/%{app_id}.desktop
(cd packaging/icons/hicolor && find . -name '*.svg' -print0 | while IFS= read -r -d '' icon; do
    install -Dpm0644 "$icon" "%{buildroot}%{_datadir}/icons/hicolor/$icon"
done)

while read -r lang; do
    [ -z "$lang" ] && continue
    case "$lang" in \#*) continue;; esac
    install -d %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES
    msgfmt --check -o %{buildroot}%{_datadir}/locale/$lang/LC_MESSAGES/pdrive.mo po/$lang.po
done < po/LINGUAS
%find_lang pdrive

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
grep -q '^BusName=' %{buildroot}%{_datadir}/cloud-providers/%{cloud_ini}
# The extension must carry the system locale directory, not /usr/local.
! grep -q /usr/local %{buildroot}%{_libdir}/nautilus/extensions-4/libpdrive-nautilus.so
# The binaries must find the private libraries through the installed rpath only.
# and must not refer to the build tree (the SDK loads libproton_crypto.so from that path).
for b in pdrive pdrived; do
    rpath=$(readelf -d %{buildroot}%{_bindir}/$b | sed -n 's/.*R\(UN\)\?PATH.*\[\(.*\)\]/\2/p')
    test "$rpath" = "%{_libdir}/pdrive" || { echo "unexpected rpath for $b: $rpath" >&2; exit 1; }
    ! grep -q "$PWD/native" %{buildroot}%{_bindir}/$b || { echo "$b refers to the build tree" >&2; exit 1; }
    grep -q "%{_libdir}/pdrive" %{buildroot}%{_bindir}/$b
done

%post
# Migration: install-system.sh (development install) put the declaration under /usr/local
# (name before the rename: io.github.pdrive.PDrive.ini), which libcloudproviders also reads:
# the account would appear twice. Files of /usr/local belong to the administrator, so they
# are never removed from a scriptlet: warn only.
for ini in io.github.pdrive.PDrive.ini %{cloud_ini}; do
    if [ -e "/usr/local/share/cloud-providers/$ini" ]; then
        echo "pdrive-for-linux: /usr/local/share/cloud-providers/$ini duplicates the packaged declaration." >&2
        echo "pdrive-for-linux: remove it with: sudo rm /usr/local/share/cloud-providers/$ini" >&2
    fi
done

%files -f pdrive.lang
%license LICENSE THIRD_PARTY_NOTICES
%license THIRD_PARTY_CRATES.txt
%license pdrive-native-linux-x64-%{native_id}/LICENSE-*
%doc pdrive-native-linux-x64-%{native_id}/SOURCES.txt
%doc licenses/*.tsv
%{_bindir}/pdrive
%{_bindir}/pdrived
%{_bindir}/pdrive-app
%{_mandir}/man1/pdrive.1*
%{_mandir}/man1/pdrived.1*
%{_mandir}/man1/pdrive-app.1*
%dir %{_libdir}/pdrive
%{_libdir}/pdrive/libproton_drive_sdk.so
%{_libdir}/pdrive/libproton_crypto.so
%{_libdir}/nautilus/extensions-4/libpdrive-nautilus.so
%{_datadir}/cloud-providers/%{cloud_ini}
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/icons/hicolor/*/*/*.svg

%changelog
* Wed Oct 28 2026 Bertrand PRESLES <bpresles@gmail.com> - 0.1.0-1
- Initial package
