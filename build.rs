use std::{env, path::PathBuf, process::Command};

fn main() {
    println!("cargo:rerun-if-changed=src/macos_native.m");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos")
        || env::var_os("CARGO_FEATURE_DESKTOP").is_none()
    {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out.join("macos_native.o");
    let status = Command::new("xcrun")
        .args([
            "clang",
            "-fobjc-arc",
            "-fblocks",
            "-mmacosx-version-min=15.0",
            "-c",
            "src/macos_native.m",
            "-o",
        ])
        .arg(&object)
        .status()
        .expect("Xcode command line tools are required");
    assert!(status.success(), "compiling macOS window support failed");
    let status = Command::new("ar")
        .arg("crs")
        .arg(out.join("libsd_macos.a"))
        .arg(object)
        .status()
        .expect("run ar");
    assert!(status.success());
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=sd_macos");
    for framework in ["AppKit", "Carbon"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
