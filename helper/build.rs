use std::path::PathBuf;
use std::process::Command;

fn main() {
    // SDL's @available checks need compiler-rt's ___isPlatformVersionAtLeast. Apple's clang adds it
    // even under rustc's -nodefaultlibs; nixpkgs' clang doesn't, so name it.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let Ok(out) = Command::new(cc).arg("-print-resource-dir").output() else {
        return;
    };
    let dir = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim()).join("lib/darwin");
    if dir.join("libclang_rt.osx.a").exists() {
        println!("cargo:rustc-link-search=native={}", dir.display());
        println!("cargo:rustc-link-lib=static=clang_rt.osx");
    }
}
