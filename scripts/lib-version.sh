# Sourced by the packaging scripts: single version source and native archive identity.
# Provides: ROOT, VERSION (Cargo.toml workspace.package.version), NATIVE_ID (pinned revisions
# of scripts/build-native.sh), NATIVE_ARCHIVE (file name).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

VERSION="$(awk '/^\[workspace\.package\]/{f=1;next} /^\[/{f=0} f && /^version *=/{gsub(/"/,"",$3); print $3; exit}' "$ROOT/Cargo.toml")"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || { echo "Unusable version in Cargo.toml: '$VERSION' (expected X.Y.Z)" >&2; exit 1; }

SDK_REV="$(sed -n 's/^SDK_REV="\([0-9a-f]\{40\}\)"/\1/p' "$ROOT/scripts/build-native.sh")"
CRYPTO_REV="$(sed -n 's/^CRYPTO_REV="\([0-9a-f]\{40\}\)"/\1/p' "$ROOT/scripts/build-native.sh")"
[[ -n "$SDK_REV" && -n "$CRYPTO_REV" ]] || { echo "Pinned revisions not found in scripts/build-native.sh" >&2; exit 1; }
NATIVE_ID="${SDK_REV:0:7}-${CRYPTO_REV:0:7}"
NATIVE_ARCHIVE="pdrive-native-linux-x64-$NATIVE_ID.tar.xz"

# Fixed timestamp of archive members (reproducible, and not 1970 which lintian rejects):
# SOURCE_DATE_EPOCH, else the date of the last commit.
MTIME="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT" log -1 --format=%ct)}"
