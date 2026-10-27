use std::path::PathBuf;

fn main() {
    // Directory holding libproton_drive_sdk.so and libproton_crypto.so (see scripts/build-native.sh).
    let lib_dir = match std::env::var("PDSDK_LIB_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../native/linux-x64"),
    };
    let lib_dir = lib_dir.canonicalize().unwrap_or(lib_dir);

    println!("cargo:rerun-if-env-changed=PDSDK_LIB_DIR");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=dylib=proton_drive_sdk");
    println!("cargo:rustc-env=PDSDK_DEFAULT_LIB_DIR={}", lib_dir.display());
    // Exposed to dependent build scripts as DEP_PROTON_DRIVE_SDK_LIB_DIR (e.g. to set an rpath).
    println!("cargo:lib_dir={}", lib_dir.display());
}
