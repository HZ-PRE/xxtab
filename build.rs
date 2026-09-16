fn main() {
    println!("cargo:rerun-if-changed=windows.manifest");
    println!("cargo:rerun-if-changed=assets/xxtab.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_FEATURE_WINDOWS_GUI").is_ok()
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        winresource::WindowsResource::new()
            .set_icon("assets/xxtab.ico")
            .compile()
            .expect("failed to embed xxtab icon");
        let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap())
            .join("windows.manifest");
        println!("cargo:rustc-link-arg-bin=xxtab-gui=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bin=xxtab-gui=/MANIFESTUAC:NO");
        println!(
            "cargo:rustc-link-arg-bin=xxtab-gui=/MANIFESTINPUT:{}",
            manifest.display()
        );
    }
}
