// SPDX-License-Identifier: MIT

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    assert_eq!(
        target, "i686-pc-windows-msvc",
        "FontBridge preserves the Windows x86 stdcall ABI"
    );
    println!("cargo:rerun-if-changed=build.rs");
    let exports =
        std::path::PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap_or_default())
            .join("FontBridge.def");
    println!("cargo:rerun-if-changed={}", exports.display());
    // Cargo's intermediate DLL filename differs from the public component name
    // in the maintained DEF. Only its EXPORTS section affects the native ABI.
    let contents = std::fs::read_to_string(&exports)
        .unwrap_or_else(|error| panic!("could not read FontBridge exports: {error}"));
    let (_, entries) = contents
        .split_once("EXPORTS")
        .unwrap_or_else(|| panic!("FontBridge.def is missing its EXPORTS section"));
    let linker_exports = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap_or_default())
        .join("FontBridge.exports.def");
    std::fs::write(&linker_exports, format!("EXPORTS{entries}"))
        .unwrap_or_else(|error| panic!("could not prepare FontBridge exports: {error}"));
    println!(
        "cargo:rustc-link-arg-cdylib=/DEF:{}",
        linker_exports.display()
    );
    println!("cargo:rustc-link-arg-cdylib=/DYNAMICBASE");
    println!("cargo:rustc-link-arg-cdylib=/NXCOMPAT");
    // Consumers load the four exports dynamically; no import library ships.
    println!("cargo:rustc-link-arg-cdylib=/NOIMPLIB");
    println!("cargo:rustc-link-arg-cdylib=/NOEXP");
}
