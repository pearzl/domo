//! Build and repository version metadata exposed by the health endpoint.

use std::path::Path;
use std::process::Command;
use std::sync::OnceLock;

/// Package version declared in `server/Cargo.toml`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Commit short hash captured by `build.rs` at compile time.
const BUILD_GIT_HASH: &str = env!("DOMO_GIT_HASH");

/// Commit short hash of the repository containing the server source.
///
/// The hash is resolved from the build-time manifest directory so a binary
/// built from a Git checkout reports the checked-out commit. Without a Git
/// checkout, or when `git` is unavailable, the build-time value is used; it is
/// `unknown` when neither source can provide a hash.
pub fn git_hash() -> &'static str {
    static HASH: OnceLock<String> = OnceLock::new();
    HASH.get_or_init(|| detect_git_hash().unwrap_or_else(|| BUILD_GIT_HASH.to_string()))
        .as_str()
}

fn detect_git_hash() -> Option<String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(hash) = git_hash_at(manifest) {
        return Some(hash);
    }
    let cwd = std::env::current_dir().ok()?;
    git_hash_at(&cwd)
}

fn git_hash_at(dir: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let hash = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if hash.is_empty() || hash == "HEAD" {
        None
    } else {
        Some(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_matches_package() {
        assert_eq!(VERSION, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn git_hash_is_short_or_unknown() {
        let hash = git_hash();
        assert!(!hash.is_empty());
        assert!(
            hash == "unknown"
                || (hash.len() <= 40 && hash.chars().all(|ch| ch.is_ascii_hexdigit()))
        );
    }
}
