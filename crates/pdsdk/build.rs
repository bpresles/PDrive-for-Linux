// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
use std::path::PathBuf;

fn main() {
    // Directory holding libproton_drive_sdk.so and libproton_crypto.so (see scripts/build-native.sh).
    let lib_dir = match std::env::var("PDSDK_LIB_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../native/linux-x64"),
    };
    let lib_dir = lib_dir.canonicalize().unwrap_or(lib_dir);
    // Where the libraries are found at run time (rpath of the binaries, and the path given to
    // the SDK for libproton_crypto.so): the installed directory for packages, which link against
    // a copy in the build tree. Defaults to the directory linked against.
    let runtime_dir = std::env::var("PDSDK_RUNTIME_LIB_DIR").map(PathBuf::from).unwrap_or_else(|_| lib_dir.clone());

    println!("cargo:rerun-if-env-changed=PDSDK_LIB_DIR");
    println!("cargo:rerun-if-env-changed=PDSDK_RUNTIME_LIB_DIR");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=proton_drive_sdk");
    println!("cargo:rustc-env=PDSDK_DEFAULT_LIB_DIR={}", runtime_dir.display());
    // Development convenience: tests of this crate find the library at run time without LD_LIBRARY_PATH.
    println!("cargo:rustc-link-arg-tests=-Wl,-rpath,{}", lib_dir.display());
    // Exposed to dependent build scripts as DEP_PROTON_DRIVE_SDK_LIB_DIR (e.g. to set an rpath).
    println!("cargo:lib_dir={}", runtime_dir.display());
}
