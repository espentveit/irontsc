//! Embed the app icon into `irontsc.exe`, so Explorer, shortcuts and the taskbar show it
//! without a separate `.ico` beside the binary.

fn main() {
    println!("cargo:rerun-if-changed=irontsc.rc");
    println!("cargo:rerun-if-changed=packaging/assets/irontsc.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_resource::compile("irontsc.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("compile irontsc.rc");
    }
}
