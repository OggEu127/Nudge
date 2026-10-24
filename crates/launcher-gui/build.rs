//! Embeds the Nudge icon and Windows version metadata into the GUI executable.

use std::{env, ffi::OsStr, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let icon =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"))
            .join("../../assets/branding/nudge-icon-blue-violet.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    assert!(icon.is_file(), "missing icon asset: {}", icon.display());
    if env::var_os("CARGO_CFG_TARGET_OS").as_deref() != Some(OsStr::new("windows")) {
        return;
    }
    let icon = icon.to_string_lossy().replace('\\', "/");
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(&icon);
    resource.set("ProductName", "Nudge");
    resource.set("FileDescription", "Nudge Minecraft Launcher");
    resource.set("OriginalFilename", "nudge-gui.exe");
    resource
        .compile_for("nudge-gui")
        .expect("the Nudge GUI Windows resource must compile");
}
