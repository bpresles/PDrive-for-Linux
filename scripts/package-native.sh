#!/usr/bin/env bash
# Packs the prebuilt native libraries (output of scripts/build-native.sh) into the archive
# used as Source1 by the RPM and by the DEB build:
#   dist/pdrive-native-linux-x64-<sdk7>-<crypto7>.tar.xz  (+ .sha256)
# The archive holds the two .so, their SHA-256 sums, the pinned source revisions and the
# licence files of the upstream sources (MIT, Proton AG). Reproducible: sorted, fixed mtime.
set -euo pipefail
source "$(dirname "${BASH_SOURCE[0]}")/lib-version.sh"

SRC="$ROOT/native/linux-x64"
for f in libproton_drive_sdk.so libproton_crypto.so; do
    [[ -f "$SRC/$f" ]] || { echo "$SRC/$f missing: run scripts/build-native.sh first" >&2; exit 1; }
done
for l in "$ROOT/third_party/sdk/LICENSE.md" "$ROOT/third_party/dotnet-crypto/LICENSE.txt"; do
    [[ -f "$l" ]] || { echo "$l missing (third_party/ is filled by scripts/build-native.sh)" >&2; exit 1; }
done

STAGE="$(mktemp -d)"; trap 'rm -rf "$STAGE"' EXIT
D="$STAGE/pdrive-native-linux-x64-$NATIVE_ID"
mkdir -p "$D" "$ROOT/dist"
install -m0755 "$SRC/libproton_drive_sdk.so" "$SRC/libproton_crypto.so" "$D/"
cp "$ROOT/third_party/sdk/LICENSE.md" "$D/LICENSE-proton-sdk.md"
cp "$ROOT/third_party/dotnet-crypto/LICENSE.txt" "$D/LICENSE-proton-dotnet-crypto.txt"
cat > "$D/SOURCES.txt" <<EOT
Prebuilt by scripts/build-native.sh (NativeAOT, .NET SDK from the build environment).
libproton_drive_sdk.so  https://github.com/ProtonDriveApps/sdk.git          $SDK_REV
libproton_crypto.so     https://github.com/ProtonDriveApps/dotnet-crypto.git $CRYPTO_REV
Licence of both: MIT, Copyright Proton AG (see LICENSE-* files).
EOT
(cd "$D" && sha256sum *.so > SHA256SUMS)

OUT="$ROOT/dist/$NATIVE_ARCHIVE"
tar -C "$STAGE" --sort=name --mtime="@$MTIME" --owner=0 --group=0 --numeric-owner -cf - "pdrive-native-linux-x64-$NATIVE_ID" | xz -9e -T1 > "$OUT"
(cd "$ROOT/dist" && sha256sum "$NATIVE_ARCHIVE" > "$NATIVE_ARCHIVE.sha256")
echo "==> $OUT"; cat "$ROOT/dist/$NATIVE_ARCHIVE.sha256"
