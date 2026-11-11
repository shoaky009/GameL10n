use std::{path::Path, process::Command};

fn git_output(args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!value.is_empty()).then_some(value)
}

fn build_git_hash() {
    let mut paths = vec!["HEAD".to_owned(), "packed-refs".to_owned()];
    if let Some(reference) = git_output(&["symbolic-ref", "--quiet", "HEAD"]) {
        paths.push(reference);
    }
    for name in paths {
        if let Some(path) = git_output(&["rev-parse", "--git-path", &name]) {
            let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
            if name == "packed-refs" && !path.exists() {
                continue;
            }
            // A packed branch acquires a loose reference on its next commit.
            if let Some(watched) = path.ancestors().find(|path| path.exists()) {
                println!("cargo:rerun-if-changed={}", watched.display());
            }
        }
    }
    let hash = git_output(&["rev-parse", "--verify", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=GAMEL10N_BUILD_GIT_HASH={hash}");
}

fn main() {
    build_git_hash();
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
