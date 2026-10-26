fn main() {
    println!("cargo:rerun-if-changed=assets/app-icon.ico");
    println!("cargo:rerun-if-changed=src/effective_skills/copilot_peer.c");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        build_copilot_peer();
    }

    #[cfg(target_os = "windows")]
    winresource::WindowsResource::new()
        .set_icon("assets/app-icon.ico")
        .set("ProductName", "Pleat")
        .set("FileDescription", "Pleat")
        .set("LegalCopyright", "Copyright Kitter contributors")
        .compile()
        .expect("failed to embed Windows application resources");
}

fn build_copilot_peer() {
    use std::{env, path::PathBuf, process::Command};

    println!("cargo:rerun-if-env-changed=MACOSX_DEPLOYMENT_TARGET");
    let (arch, minimum) = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("aarch64") => ("arm64", "11.0"),
        Ok("x86_64") => ("x86_64", "10.13"),
        _ => panic!("unsupported macOS architecture for Copilot peer bridge"),
    };
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is required"));
    let object = out.join("copilot_peer.o");
    let library = out.join("libcopilot_peer.a");
    let deployment = env::var("MACOSX_DEPLOYMENT_TARGET").unwrap_or_else(|_| minimum.into());
    let compiled = Command::new("xcrun")
        .args(["--sdk", "macosx", "clang", "-arch", arch])
        .arg(format!("-mmacosx-version-min={deployment}"))
        .args(["-std=c11", "-Wall", "-Wextra", "-Werror", "-O2", "-fPIC", "-c"])
        .arg("src/effective_skills/copilot_peer.c")
        .arg("-o")
        .arg(&object)
        .status()
        .expect("failed to run SDK clang for Copilot peer bridge");
    assert!(compiled.success(), "failed to compile Copilot peer bridge");
    let archived = Command::new("xcrun")
        .args(["--sdk", "macosx", "ar", "crs"])
        .arg(&library)
        .arg(&object)
        .status()
        .expect("failed to run SDK ar for Copilot peer bridge");
    assert!(archived.success(), "failed to archive Copilot peer bridge");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=copilot_peer");
    println!("cargo:rustc-link-lib=dylib=proc");
}
