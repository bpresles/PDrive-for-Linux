# Sourced by the scripts: architecture of the machine running them (override with
# PDSDK_ARCH=x64|arm64, e.g. to cross-build or to build the packages of another architecture).
# Provides: ARCH (x64|arm64), RID (.NET runtime identifier, also the suffix of native/ and of the
# native archive), RUST_TARGET, APPIMAGE_ARCH (x86_64|aarch64), NATIVE_DIR (native libraries).
_machine="${PDSDK_ARCH:-$(uname -m)}"
case "$_machine" in
    x86_64 | amd64 | x64) ARCH="x64" RUST_TARGET="x86_64-unknown-linux-gnu" APPIMAGE_ARCH="x86_64" ;;
    aarch64 | arm64) ARCH="arm64" RUST_TARGET="aarch64-unknown-linux-gnu" APPIMAGE_ARCH="aarch64" ;;
    *) echo "Unsupported architecture: $_machine" >&2; exit 1 ;;
esac
unset _machine
RID="linux-$ARCH"
_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NATIVE_DIR="$_root/native/$RID"
unset _root
