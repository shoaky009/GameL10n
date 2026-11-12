fn main() {
    println!("cargo:rerun-if-changed=assets/GameL10n.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winresource::WindowsResource::new()
            .set_icon("assets/GameL10n.ico")
            .set("ProductName", "GameL10n")
            .set("FileDescription", "GameL10n")
            .compile()
            .expect("Windows application icon compilation failed");
    }
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    if let Err(error) = slint_build::compile_with_config("ui/main.slint", config) {
        eprintln!("Slint UI compilation failed: {error}");
        std::process::exit(1);
    }
}
