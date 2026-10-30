#!/usr/bin/env bash
# Fails unless Cargo.toml, Cargo.lock, the RPM spec, debian/changelog (and the tag given as $1,
# with or without the leading "v") all carry the same version, and the native archive identity in
# the spec matches the revisions pinned in build-native.sh.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib-version.sh"

fail=0
check() { if [[ "$2" != "$VERSION" ]]; then echo "MISMATCH: $1 is '$2', Cargo.toml has '$VERSION'" >&2; fail=1; fi; }

check "Cargo.lock" "$(sed -n '/^name = "pdrive"$/{n;s/^version = "\(.*\)"/\1/p;}' "$ROOT/Cargo.lock")"
check "RPM spec Version" "$(sed -n 's/^Version: *//p' "$ROOT/packaging/rpm/pdrive-for-linux.spec")"
check "debian/changelog" "$(sed -n '1s/^[^(]*(\([0-9.]*\)-[^)]*).*/\1/p' "$ROOT/debian/changelog")"
if [[ -n "${1:-}" ]]; then check "tag" "${1#v}"; fi

spec_native="$(sed -n 's/^%global native_id *//p' "$ROOT/packaging/rpm/pdrive-for-linux.spec")"
if [[ "$spec_native" != "$NATIVE_ID" ]]; then
    echo "MISMATCH: spec native_id is '$spec_native', build-native.sh pins '$NATIVE_ID'" >&2; fail=1
fi
[[ $fail -eq 0 ]] && echo "Version $VERSION consistent (native $NATIVE_ID)."
exit $fail
