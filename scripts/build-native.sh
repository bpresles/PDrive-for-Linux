#!/usr/bin/env bash
# Builds the native libraries the Rust client links against:
#   - libproton_crypto.so    (Proton's Rust OpenPGP/SRP FFI, from ProtonDriveApps/dotnet-crypto)
#   - libproton_drive_sdk.so (Proton Drive C# SDK compiled with NativeAOT, from ProtonDriveApps/sdk)
#
# Outputs go to ./native/linux-x64. Sources are cloned into ./third_party at pinned commits.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
THIRD_PARTY="$ROOT/third_party"
OUT="$ROOT/native/linux-x64"
RID="linux-x64"

SDK_REPO="https://github.com/ProtonDriveApps/sdk.git"
SDK_REV="b68d64bd9f9c1f24be521c8b423c26f5dca1c681"
CRYPTO_REPO="https://github.com/ProtonDriveApps/dotnet-crypto.git"
CRYPTO_REV="fc2c10881e6fb44fc8a181c8db99cba73d06dfdb"

DOTNET="${DOTNET:-$(command -v dotnet || echo "$HOME/.dotnet/dotnet")}"
export DOTNET_CLI_TELEMETRY_OPTOUT=1
export DOTNET_NOLOGO=1

fetch() {
    local repo="$1" rev="$2" dir="$3"
    if [[ ! -d "$dir/.git" ]]; then
        git init -q "$dir"
        git -C "$dir" remote add origin "$repo"
    fi
    if [[ "$(git -C "$dir" rev-parse HEAD 2>/dev/null)" != "$rev" ]]; then
        git -C "$dir" fetch -q --depth 1 origin "$rev"
        git -C "$dir" checkout -q --force FETCH_HEAD
    fi
}

mkdir -p "$THIRD_PARTY" "$OUT"
fetch "$SDK_REPO" "$SDK_REV" "$THIRD_PARTY/sdk"
fetch "$CRYPTO_REPO" "$CRYPTO_REV" "$THIRD_PARTY/dotnet-crypto"

# The SDK selects the crypto package version from its backend; we build the Rust backend.
CRYPTO_VERSION="$(sed -n 's:.*<RustCryptoVersion>\(.*\)</RustCryptoVersion>.*:\1:p' "$THIRD_PARTY/sdk/config/cs/Packages.props")"
[[ -n "$CRYPTO_VERSION" ]] || { echo "RustCryptoVersion not found in the SDK sources" >&2; exit 1; }
FEED="$THIRD_PARTY/nuget-local"

echo "==> Building libproton_crypto ($RID)"
CRYPTO_FFI="$THIRD_PARTY/dotnet-crypto/src/rust/proton-crypto-ffi"
# Run from the crate directory so Cargo picks up its .cargo/config.toml (declares the "proton" registry).
(cd "$CRYPTO_FFI" && cargo build --release --target x86_64-unknown-linux-gnu)
mkdir -p "$THIRD_PARTY/dotnet-crypto/bin/runtimes/$RID/native"
cp "$CRYPTO_FFI/target/x86_64-unknown-linux-gnu/release/libproton_crypto."{a,so} \
    "$THIRD_PARTY/dotnet-crypto/bin/runtimes/$RID/native/"

echo "==> Packing Proton.Cryptography $CRYPTO_VERSION into the local feed"
"$DOTNET" pack -c Release -p:Version="$CRYPTO_VERSION" \
    "$THIRD_PARTY/dotnet-crypto/src/dotnet/Proton.Cryptography.csproj" --output "$FEED"
# Never reuse a stale copy of the package from the global NuGet cache.
rm -rf "$HOME/.nuget/packages/proton.cryptography/$CRYPTO_VERSION"

# The SDK maps Proton.* packages to a "Proton" source that is not public: point it at our feed.
cat > "$THIRD_PARTY/sdk/nuget.config" <<EOF
<?xml version="1.0" encoding="utf-8"?>
<configuration>
  <packageSources>
    <clear />
    <add key="nuget.org" value="https://api.nuget.org/v3/index.json" />
    <add key="Proton" value="$FEED" />
  </packageSources>
  <packageSourceMapping>
    <packageSource key="Proton">
      <package pattern="Proton.*" />
    </packageSource>
    <packageSource key="nuget.org">
      <package pattern="*" />
    </packageSource>
  </packageSourceMapping>
</configuration>
EOF

echo "==> Publishing libproton_drive_sdk ($RID, NativeAOT)"
"$DOTNET" publish -c Release -r "$RID" -p:CryptoBackend=rust \
    "$THIRD_PARTY/sdk/client/cs/src/Proton.Drive.Sdk.CExports/Proton.Drive.Sdk.CExports.csproj" \
    -o "$THIRD_PARTY/sdk-publish"

cp "$THIRD_PARTY/sdk-publish/libproton_drive_sdk.so" "$OUT/"
cp "$THIRD_PARTY/dotnet-crypto/bin/runtimes/$RID/native/libproton_crypto.so" "$OUT/"
cp "$THIRD_PARTY/sdk/client/cs/src/protos/proton.drive.sdk.proto" "$ROOT/native/"
echo "==> Done: $(ls "$OUT")"
