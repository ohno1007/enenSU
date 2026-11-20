fn main() {
    #[cfg(feature = "desktop")]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        println!("cargo:rerun-if-changed=desktop/windows.rc");
        println!("cargo:rerun-if-changed=desktop/icon.ico");
        embed_resource::compile_for(
            "desktop/windows.rc",
            ["enenSU-Desktop"],
            embed_resource::NONE,
        )
        .manifest_required()
        .expect("compile Windows resources");
    }
}
