#!/usr/bin/env bash
# Builds the packages locally, in a container (podman, else docker), without network inside
# the package build itself (vendored crates + prebuilt native libraries):
#   scripts/build-packages.sh [rpm|deb]      (default: both)
# Prerequisites (outside the container): native/linux-x64/*.so (scripts/build-native.sh, or
# the native archive of a release unpacked there), cargo, jq and git for `cargo vendor`.
# Results in dist/. Nothing is installed on the host.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib-version.sh"
cd "$ROOT"

WHAT="${1:-all}"
RPM_IMAGE="${RPM_IMAGE:-registry.fedoraproject.org/fedora:44}"
DEB_IMAGE="${DEB_IMAGE:-docker.io/library/debian:trixie}"
ENGINE="$(command -v podman || command -v docker || true)"
[[ -n "$ENGINE" ]] || { echo "podman or docker required" >&2; exit 1; }

"$ROOT/scripts/check-version.sh"
"$ROOT/scripts/package-native.sh"
"$ROOT/scripts/make-source-tarball.sh"

run() { # run <image> <script-in-container>
    "$ENGINE" run --rm -v "$ROOT/dist:/dist:Z" -v "$ROOT/scripts/container:/ci:ro,Z" -e VERSION="$VERSION" -e NATIVE_ARCHIVE="$NATIVE_ARCHIVE" "$1" bash -euxo pipefail "/ci/$2"
}

case "$WHAT" in
    rpm|all) run "$RPM_IMAGE" build-rpm.sh ;;&
    deb|all) run "$DEB_IMAGE" build-deb.sh ;;&
    rpm|deb|all) ;;
    *) echo "usage: $0 [rpm|deb]" >&2; exit 1 ;;
esac
(cd dist && sha256sum *.rpm *.deb 2>/dev/null | tee SHA256SUMS-packages)
