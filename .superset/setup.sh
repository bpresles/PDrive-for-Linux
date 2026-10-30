#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Bertrand Presles
# Paradev workspace setup, run in each new worktree. The native libraries the Rust crates link
# against are gitignored: they are copied from the main checkout ($SUPERSET_ROOT_PATH), as
# building them (scripts/build-native.sh) takes several minutes and needs the .NET SDK.
set -euo pipefail

SRC="${SUPERSET_ROOT_PATH:?SUPERSET_ROOT_PATH is not set}"
NATIVE="native/linux-x64"
LIBS=(libproton_drive_sdk.so libproton_crypto.so)

# Warns when the libraries were built from another revision than the one this branch pins.
check_pin() { # <directory under third_party/> <pinned revision>
    local built
    built="$(git -C "$SRC/third_party/$1" rev-parse HEAD 2>/dev/null || echo unknown)"
    [[ "$built" == "$2" ]] \
        || echo "warning: $NATIVE was built from $1 $built, this branch pins $2: run scripts/build-native.sh" >&2
}

if [[ -f "$SRC/$NATIVE/${LIBS[0]}" && -f "$SRC/$NATIVE/${LIBS[1]}" ]]; then
    mkdir -p "$NATIVE"
    cp --reflink=auto "${LIBS[@]/#/$SRC/$NATIVE/}" "$NATIVE/"
    source scripts/lib-version.sh # SDK_REV, CRYPTO_REV
    check_pin sdk "$SDK_REV"
    check_pin dotnet-crypto "$CRYPTO_REV"
    echo "==> Native libraries copied from $SRC/$NATIVE"
else
    echo "warning: no native libraries in $SRC/$NATIVE: run scripts/build-native.sh (several minutes)" >&2
fi

# Crates into the shared Cargo cache, so that builds in this worktree need no network.
cargo fetch --locked
