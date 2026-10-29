#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Copyright (C) 2026 Bertrand Presles
# Regenerates the inventory of third-party licences in licenses/ (tab-separated, sorted, no
# dates: the output only changes when a dependency does):
#   rust-pdrive.tsv         Rust crates of the PDrive programs (Cargo.lock, Linux target, no dev-dependencies)
#   rust-proton-crypto.tsv  Rust crates statically linked into libproton_crypto.so
#                           (third_party/dotnet-crypto, after scripts/build-native.sh)
#   nuget-sdk.tsv           NuGet packages restored for the NativeAOT SDK library
#                           (third_party/sdk, after scripts/build-native.sh; nuspec files in ~/.nuget/packages)
# Rust lists come from cargo-about (cargo install cargo-about --locked), which fails when a
# crate is not under one of the licences accepted in scripts/audit-licenses/about.toml: such a
# crate must then be reviewed by a human. The licence texts are not generated or copied here:
# they are the files delivered by each crate/package (vendor/ of the source archive, NuGet cache).
# With --texts <file>, also writes the licence texts of the Rust crates of the PDrive programs
# (as delivered in each crate, grouped by identical text, with the list of crates using it): the
# bundle to ship with binaries, since MIT, BSD and Apache-2.0 require their notices to be kept.
# Columns of the Rust lists: crate, declared licence expression, licence retained for the audit.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CFG="$ROOT/scripts/audit-licenses"
OUT="$ROOT/licenses"
SDK_ASSETS="$ROOT/third_party/sdk/client/cs/src/Proton.Drive.Sdk.CExports/obj/project.assets.json"
CRYPTO_MANIFEST="$ROOT/third_party/dotnet-crypto/src/rust/proton-crypto-ffi/Cargo.toml"
NUGET_ROOT="${NUGET_PACKAGES:-$HOME/.nuget/packages}"
export LC_ALL=C

TEXTS=""
if [[ "${1:-}" == "--texts" ]]; then TEXTS="${2:?usage: $0 [--texts <file>]}"; fi

command -v cargo-about >/dev/null || { echo "cargo-about missing: cargo install cargo-about --locked" >&2; exit 1; }
command -v jq >/dev/null || { echo "jq missing" >&2; exit 1; }
mkdir -p "$OUT"

rust_list() { # rust_list <output> <cargo-about arguments...>
    local out="$1"; shift
    cargo about generate -c "$CFG/about.toml" "$@" "$CFG/about.hbs" 2>/dev/null \
        | awk -F'\t' 'NF{ if ($2 == "") $2 = "(no licence field)"; print $1 "\t" $2 "\t" $3 }' OFS='\t' | sort -u > "$out.tmp"
    mv "$out.tmp" "$out"
    echo "==> $out ($(wc -l < "$out") crates)"
}

(cd "$ROOT" && rust_list "$OUT/rust-pdrive.tsv" --workspace --manifest-path "$ROOT/Cargo.toml")

if [[ -n "$TEXTS" ]]; then
    (cd "$ROOT" && cargo about generate -c "$CFG/about.toml" --workspace --manifest-path "$ROOT/Cargo.toml" "$CFG/texts.hbs" 2>/dev/null) > "$TEXTS"
    echo "==> $TEXTS ($(grep -c '^=====' "$TEXTS") distinct licence texts)"
fi

if [[ -f "$CRYPTO_MANIFEST" ]]; then
    rust_list "$OUT/rust-proton-crypto.tsv" --manifest-path "$CRYPTO_MANIFEST"
else
    echo "skipped rust-proton-crypto.tsv: run scripts/build-native.sh first" >&2
fi

if [[ -f "$SDK_ASSETS" ]]; then
    {
        printf 'package\tversion\tlicence (nuspec)\tcopyright (nuspec)\trole\n'
        # role: "linked" = has a runtime asset in the linux-x64 closure (compiled into the library),
        # or is the NativeAOT runtime pack; "build" = restored for the build only.
        jq -r '
          (.targets["net10.0/linux-x64"] | to_entries[] | select(.value.type == "package")
            | [.key, (if ((.value.runtime // {}) | length) > 0 then "linked" else "build" end)] | @tsv),
          (.project.frameworks[].downloadDependencies[] | [.name + "/" + (.version | gsub("[\\[\\] ]"; "") | split(",")[0]),
            (if (.name | test("NativeAOT")) then "linked" else "build" end)] | @tsv)' "$SDK_ASSETS" \
        | sort -u | while IFS=$'\t' read -r id role; do
            name="${id%/*}"; ver="${id#*/}"
            dir="$NUGET_ROOT/${name,,}/$ver"
            nuspec="$(ls "$dir"/*.nuspec 2>/dev/null | head -1 || true)"
            if [[ -z "$nuspec" ]]; then printf '%s\t%s\t(nuspec not found)\t\t%s\n' "$name" "$ver" "$role"; continue; fi
            lic="$(sed -n 's:.*<license type="expression">\(.*\)</license>.*:\1:p' "$nuspec")"
            if [[ -z "$lic" ]]; then
                f="$(sed -n 's:.*<license type="file">\(.*\)</license>.*:\1:p' "$nuspec" | tr '\\' '/')"
                if [[ -n "$f" && -f "$dir/$f" ]]; then lic="file $f: $(grep -m1 . "$dir/$f" | tr -d '\r')"; else lic="(none in nuspec)"; fi
            fi
            cp="$(sed -n 's:.*<copyright>\(.*\)</copyright>.*:\1:p' "$nuspec")"
            printf '%s\t%s\t%s\t%s\t%s\n' "$name" "$ver" "$lic" "$cp" "$role"
        done
    } > "$OUT/nuget-sdk.tsv.tmp"
    mv "$OUT/nuget-sdk.tsv.tmp" "$OUT/nuget-sdk.tsv"
    echo "==> $OUT/nuget-sdk.tsv ($(($(wc -l < "$OUT/nuget-sdk.tsv") - 1)) packages)"
else
    echo "skipped nuget-sdk.tsv: run scripts/build-native.sh first" >&2
fi
