#!/usr/bin/env bash
# Builds dist/pdrive-for-linux-<version>.tar.xz (Source0 of the RPM, orig tarball of the DEB):
# the project files plus the Rust dependencies vendored with `cargo vendor` (offline builds),
# THIRD_PARTY_CRATES.txt (licence of each vendored crate) and .cargo/config.toml.
# Uses the files of the working tree that are tracked or not ignored (so it also works before
# the commit). Needs network access once, for `cargo vendor`.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib-version.sh"
cd "$ROOT"

NAME="pdrive-for-linux-$VERSION"
STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
D="$STAGE/$NAME"
mkdir -p "$D" dist

git ls-files -co --exclude-standard -z | while IFS= read -r -d '' f; do
    [[ -f "$f" ]] && echo "$f"
done | grep -v '^\(third_party\|native/linux-x64\|dist\)/' | tar -T - -cf - | tar -C "$D" -xf -

mkdir -p "$D/.cargo"
cargo vendor --locked "$D/vendor" 2>/dev/null | sed "s#^directory = .*#directory = \"vendor\"#" > "$D/.cargo/config.toml"

cargo metadata --locked --format-version 1 | jq -r '
  .packages[] | select(.source != null) | "\(.name) \(.version)\t\(.license // "see vendor/"+.name+"/")"' \
  | sort -u > "$D/THIRD_PARTY_CRATES.txt"

OUT="dist/$NAME.tar.xz"
tar -C "$STAGE" --sort=name --mtime="@$MTIME" --owner=0 --group=0 --numeric-owner -cf - "$NAME" | xz -6 -T0 > "$OUT"
(cd dist && sha256sum "$NAME.tar.xz" > "$NAME.tar.xz.sha256")
echo "==> $OUT ($(du -h "$OUT" | cut -f1))"
