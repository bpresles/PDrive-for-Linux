// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Bertrand Presles
fn main() {
    // Development convenience: locate libproton_drive_sdk.so at run time without LD_LIBRARY_PATH.
    if let Ok(dir) = std::env::var("DEP_PROTON_DRIVE_SDK_LIB_DIR") {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{dir}");
    }
}
