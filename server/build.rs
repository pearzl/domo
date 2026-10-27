//! Injects build metadata consumed by the health endpoint.

use std::path::Path;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let hash = git(manifest, &["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=DOMO_GIT_HASH={hash}");
    watch_git_metadata(manifest);
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn watch_git_metadata(dir: &Path) {
    let Some(head) = git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-path", "HEAD"],
    ) else {
        return;
    };
    println!("cargo:rerun-if-changed={head}");
    if let Some(reference) = git(dir, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git(
            dir,
            &[
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                &reference,
            ],
        ) {
            println!("cargo:rerun-if-changed={path}");
        }
    }
    if let Some(packed) = git(
        dir,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "packed-refs",
        ],
    ) {
        println!("cargo:rerun-if-changed={packed}");
    }
}
