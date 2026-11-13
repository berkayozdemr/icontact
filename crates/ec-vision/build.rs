//! Build script: runs before this crate is compiled.
//!
//! On macOS the prebuilt ONNX Runtime library calls a helper function
//! (`__isPlatformVersionAtLeast`) that lives in Apple clang's runtime library.
//! Rust's linker line leaves that library out, so a release build stops with
//! "Undefined symbols". Asking clang where the library is and adding it fixes that.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let dir = Command::new("cc")
        .arg("--print-runtime-dir")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|d| !d.is_empty());
    match dir {
        Some(dir) => {
            println!("cargo:rustc-link-search=native={dir}");
            println!("cargo:rustc-link-lib=static=clang_rt.osx");
        }
        None => println!(
            "cargo:warning=could not ask `cc` for its runtime folder; a release build may fail to link"
        ),
    }
}
