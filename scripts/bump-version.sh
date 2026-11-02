#!/usr/bin/env bash
# Usage: bump-version.sh patch|minor|major
# Raises the version in every place scripts/check-version.sh compares (Cargo.toml, the workspace
# crates in Cargo.lock, RPM spec, PKGBUILD, debian/changelog, with a changelog entry in both) and prints the
# new version. Does not commit or tag.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib-version.sh"
export LC_ALL=C

part="${1:-}"
IFS=. read -r major minor patch <<< "$VERSION"
case "$part" in
    patch) patch=$((patch + 1)) ;;
    minor) minor=$((minor + 1)); patch=0 ;;
    major) major=$((major + 1)); minor=0; patch=0 ;;
    *) echo "Usage: ${0##*/} patch|minor|major" >&2; exit 2 ;;
esac
new="$major.$minor.$patch"

# Same maintainer as the latest entry of debian/changelog.
maintainer="$(sed -n '1,/^ -- /s/^ -- \(.*[>]\)  .*/\1/p' "$ROOT/debian/changelog" | head -n 1)"
[[ -n "$maintainer" ]] || { echo "Maintainer not found in debian/changelog" >&2; exit 1; }
note="Release $new"

# sed -i differs between GNU and BSD: go through a temporary file.
edit() { local f="$1"; shift; sed "$@" "$f" > "$f.tmp" && mv "$f.tmp" "$f"; }

edit "$ROOT/Cargo.toml" "/^\[workspace\.package\]/,/^\[/s/^version = \".*\"/version = \"$new\"/"

# Workspace crates only: other packages of Cargo.lock can share the number.
for crate in $(sed -n 's|^members = \[\(.*\)\]|\1|p' "$ROOT/Cargo.toml" | tr -d '", ' | sed 's|crates/| |g'); do
    edit "$ROOT/Cargo.lock" "/^name = \"$crate\"\$/{n;s/^version = \".*\"/version = \"$new\"/;}"
done

edit "$ROOT/packaging/arch/PKGBUILD" "s/^pkgver=.*/pkgver=$new/"

edit "$ROOT/packaging/rpm/pdrive-for-linux.spec" \
    -e "s/^Version:\( *\).*/Version:\1$new/" \
    -e "/^%changelog/a\\
* $(date '+%a %b %d %Y') $maintainer - $new-1\\
- $note"

{
    printf 'pdrive-for-linux (%s-1) unstable; urgency=low\n\n  * %s\n\n -- %s  %s\n\n' \
        "$new" "$note" "$maintainer" "$(date '+%a, %d %b %Y %H:%M:%S %z')"
    cat "$ROOT/debian/changelog"
} > "$ROOT/debian/changelog.tmp" && mv "$ROOT/debian/changelog.tmp" "$ROOT/debian/changelog"

echo "$new"
