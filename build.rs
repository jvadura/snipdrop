//! Embeds the icon and version information into the Windows executable, so
//! shortcuts, Task Manager and the tray settings show "snipdrop" with its icon.

use std::path::Path;

const ICON: &str = "assets/snipdrop.ico";

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed={ICON}");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    if Path::new(ICON).exists() {
        resource.set_icon(ICON);
    }
    // Task Manager and the notification-area settings list show this text.
    resource.set("FileDescription", "snipdrop");
    resource.set("ProductName", "snipdrop");
    resource.set("LegalCopyright", "MIT License");

    // The resources are cosmetic: a machine without the Windows SDK resource
    // compiler should still be able to build a working program.
    if let Err(error) = resource.compile() {
        println!("cargo:warning=building without icon and version info: {error}");
    }
}
