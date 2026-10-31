//! Platform-specific path and child-process handling.
//!
//! Windows differs from Unix in three places DOMO depends on:
//! - `HOME` may be a POSIX path when the server is started from Git Bash, so
//!   the native profile directory is used as the fallback;
//! - a bare executable name is resolved through `PATHEXT`, because Rust only
//!   appends `.exe` when searching `PATH`;
//! - npm installs CLI tools as `.cmd` shims. A shim is reduced to the program
//!   it wraps so multi-line prompts never pass through `cmd.exe`.

use std::env;
#[cfg(windows)]
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The user's home directory in a form native processes can use.
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            // Git Bash exports `/c/Users/...`, which is not a Windows path.
            if home.is_absolute() {
                return Some(home);
            }
        }
        if let Some(profile) = env::var_os("USERPROFILE").map(PathBuf::from) {
            if profile.is_absolute() {
                return Some(profile);
            }
        }
        if let (Some(drive), Some(path)) = (env::var_os("HOMEDRIVE"), env::var_os("HOMEPATH")) {
            let mut home = PathBuf::from(drive);
            home.push(path);
            if home.is_absolute() {
                return Some(home);
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        env::var_os("HOME")
            .or_else(|| env::var_os("USERPROFILE"))
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
    }
}

/// Expands a leading `~`, accepting both `/` and `\` as the separator.
pub fn expand_tilde(raw: &str) -> PathBuf {
    expand_tilde_with(home_dir().as_deref(), raw)
}

/// Expands a leading `~` in an already parsed path. Non-UTF-8 paths are kept
/// unchanged so environment values are never corrupted.
pub fn expand_tilde_path(path: PathBuf) -> PathBuf {
    match path.to_str() {
        Some(text) if text == "~" || text.starts_with("~/") || text.starts_with("~\\") => {
            expand_tilde(text)
        }
        _ => path,
    }
}

fn expand_tilde_with(home: Option<&Path>, raw: &str) -> PathBuf {
    let raw = raw.trim();
    if raw == "~" {
        return home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(raw));
    }
    let rest = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\"));
    match (home, rest) {
        (Some(home), Some(rest)) => home.join(rest),
        _ => PathBuf::from(raw),
    }
}

/// Recursive directory removal that also covers trees deeper than the legacy
/// `MAX_PATH` limit on Windows.
pub async fn remove_dir_all(path: &Path) -> std::io::Result<()> {
    let path = extended_length(path);
    tokio::fs::remove_dir_all(&path).await
}

/// Absolute Windows paths in extended-length form. Relative paths, device
/// paths, and paths that already carry the `\\?\` prefix are returned
/// unchanged.
#[cfg(windows)]
fn extended_length(path: &Path) -> PathBuf {
    use std::path::Component;

    let text = path.to_string_lossy().replace('/', "\\");
    if !matches!(path.components().next(), Some(Component::Prefix(_)))
        || text.starts_with(r"\\?\")
        || text.starts_with(r"\\.\")
    {
        return path.to_path_buf();
    }
    match text.strip_prefix(r"\\") {
        Some(rest) => PathBuf::from(format!(r"\\?\UNC\{rest}")),
        None => PathBuf::from(format!(r"\\?\{text}")),
    }
}

#[cfg(not(windows))]
fn extended_length(path: &Path) -> PathBuf {
    path.to_path_buf()
}

/// A program plus any arguments that must precede the caller's arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedProgram {
    pub program: PathBuf,
    pub prefix_args: Vec<String>,
}

impl ResolvedProgram {
    fn direct(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            prefix_args: Vec::new(),
        }
    }
}

/// Resolves a command template executable into something `Command` can spawn.
pub fn resolve_program(program: &str) -> ResolvedProgram {
    #[cfg(windows)]
    {
        resolve_windows(program)
    }
    #[cfg(not(windows))]
    {
        ResolvedProgram::direct(program)
    }
}

/// Builds a command with platform-specific program resolution applied.
pub fn command(program: &str, args: &[String]) -> tokio::process::Command {
    let resolved = resolve_program(program);
    let mut command = tokio::process::Command::new(resolved.program);
    command.args(resolved.prefix_args).args(args);
    command
}

#[cfg(windows)]
fn resolve_windows(program: &str) -> ResolvedProgram {
    let Some(path) = search_path(program) else {
        return ResolvedProgram::direct(program);
    };
    if is_batch(&path) {
        if let Some(resolved) = shim_program(&path, 0) {
            return resolved;
        }
    }
    ResolvedProgram::direct(path)
}

#[cfg(windows)]
fn search_path(program: &str) -> Option<PathBuf> {
    search_path_in(program, env::var_os("PATH"), env::var("PATHEXT").ok())
}

#[cfg(windows)]
fn search_path_in(
    program: &str,
    path: Option<OsString>,
    pathext: Option<String>,
) -> Option<PathBuf> {
    let raw = Path::new(program);
    let path_like =
        program.contains('\\') || program.contains('/') || program.as_bytes().get(1) == Some(&b':');
    if path_like {
        return raw.is_file().then(|| raw.to_path_buf());
    }
    let has_extension = raw.extension().is_some();
    let extensions = pathext.unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    let extensions: Vec<&str> = extensions
        .split(';')
        .map(str::trim)
        .filter(|extension| {
            [".exe", ".com", ".bat", ".cmd"]
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
        .collect();
    let path = path?;
    for dir in env::split_paths(&path) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        if has_extension {
            let candidate = dir.join(program);
            if candidate.is_file() {
                return Some(candidate);
            }
            continue;
        }
        for extension in &extensions {
            let candidate = dir.join(format!("{program}{extension}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(windows)]
fn is_batch(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

/// Reads an npm-style batch shim and returns the program it invokes. Shims are
/// generated as `"%_prog%" "script" %*`, so the last `&`-separated segment of
/// the forwarding line is the real command.
#[cfg(windows)]
fn shim_program(path: &Path, depth: usize) -> Option<ResolvedProgram> {
    if depth > 4 {
        return None;
    }
    let text = std::fs::read_to_string(path).ok()?;
    let dir = path.parent()?;
    let dir = format!("{}\\", dir.display());
    let text = text.replace("%~dp0", &dir).replace("%dp0%", &dir);
    let prog = shim_prog_value(&text);
    for line in text.lines().rev() {
        if !line.contains("%*") {
            continue;
        }
        let command = line
            .split('&')
            .rev()
            .find(|part| part.contains("%*"))
            .unwrap_or_default()
            .trim();
        if command.is_empty() {
            continue;
        }
        let command = command.replace("%*", "");
        let command = match (&prog, command.contains("%_prog%")) {
            (Some(prog), true) => command.replace("%_prog%", prog),
            _ => command,
        };
        let mut tokens = split_shim_tokens(&command);
        // Some shims keep `IF ... (...) ELSE (...)` on one line. The last
        // parenthesized branch is the command that receives `%*`.
        if let Some(position) = tokens.iter().rposition(|token| token == "(") {
            tokens.drain(..=position);
        } else if let Some(position) = tokens.iter().rposition(|token| token.starts_with('(')) {
            tokens.drain(..position);
            tokens[0] = tokens[0][1..].to_string();
        }
        if let Some(last) = tokens.last_mut() {
            if last == ")" {
                tokens.pop();
            } else if let Some(stripped) = last.strip_suffix(')') {
                *last = stripped.to_string();
            }
        }
        if tokens.is_empty() {
            continue;
        }
        let program = tokens.remove(0);
        if program.contains('%') {
            continue;
        }
        let mut resolved = resolve_token(&program, depth + 1)?;
        resolved.prefix_args.extend(tokens);
        return Some(resolved);
    }
    None
}

#[cfg(windows)]
fn shim_prog_value(text: &str) -> Option<String> {
    let mut fallback = None;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("SET ") else {
            continue;
        };
        let Some(value) = rest
            .strip_prefix("\"_prog=")
            .and_then(|value| value.strip_suffix('"'))
            .or_else(|| rest.strip_prefix("_prog="))
        else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if Path::new(value).is_file() {
            return Some(value.to_string());
        }
        fallback = Some(value.to_string());
    }
    fallback
}

#[cfg(windows)]
fn resolve_token(program: &str, depth: usize) -> Option<ResolvedProgram> {
    let path = search_path(program).unwrap_or_else(|| PathBuf::from(program));
    if is_batch(&path) {
        return shim_program(&path, depth);
    }
    Some(ResolvedProgram::direct(path))
}

#[cfg(any(windows, test))]
fn split_shim_tokens(command: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    for ch in command.chars() {
        match ch {
            '"' => quoted = !quoted,
            ch if ch.is_whitespace() && !quoted => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            ch => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_expands_only_at_the_start() {
        let home = Path::new("home");
        assert_eq!(expand_tilde_with(Some(home), "~/repo"), home.join("repo"));
        assert_eq!(expand_tilde_with(Some(home), "~\\repo"), home.join("repo"));
        assert_eq!(expand_tilde_with(Some(home), "~"), home);
        assert_eq!(
            expand_tilde_with(Some(home), "repo/~/x"),
            PathBuf::from("repo/~/x")
        );
        assert_eq!(
            expand_tilde_path(PathBuf::from("repo/~/x")),
            PathBuf::from("repo/~/x")
        );
    }

    #[cfg(windows)]
    #[test]
    fn extended_length_prefixes_only_absolute_drive_and_unc_paths() {
        assert_eq!(
            extended_length(Path::new(r"C:\Users\pearzl\.domo\workspace\prj_1\tasks\19")),
            PathBuf::from(r"\\?\C:\Users\pearzl\.domo\workspace\prj_1\tasks\19")
        );
        assert_eq!(
            extended_length(Path::new("C:/Users/pearzl")),
            PathBuf::from(r"\\?\C:\Users\pearzl")
        );
        assert_eq!(
            extended_length(Path::new(r"\\server\share\dir")),
            PathBuf::from(r"\\?\UNC\server\share\dir")
        );
        for unchanged in [
            Path::new(r"\\?\C:\a"),
            Path::new(r"\\.\pipe\domo"),
            Path::new(r"relative\path"),
        ] {
            assert_eq!(extended_length(unchanged), unchanged);
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn remove_dir_all_accepts_trees_beyond_the_legacy_limit() {
        let dir = std::env::temp_dir().join(format!("domo-long-{}", uuid::Uuid::new_v4()));
        let mut deep = dir.clone();
        for _ in 0..6 {
            deep.push("0123456789012345678901234567890123456789");
        }
        std::fs::create_dir_all(extended_length(&deep)).unwrap();
        std::fs::write(extended_length(&deep.join("file.txt")), b"content").unwrap();
        assert!(deep.to_string_lossy().len() > 260);

        remove_dir_all(&dir).await.unwrap();

        assert!(!dir.exists());
    }

    #[test]
    fn shim_tokens_keep_quoted_paths_whole() {
        let tokens = split_shim_tokens(r#""C:\Program Files\node.exe" "C:\a b\cli.js" --flag"#);
        assert_eq!(
            tokens,
            vec![
                r"C:\Program Files\node.exe".to_string(),
                r"C:\a b\cli.js".to_string(),
                "--flag".to_string()
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn path_search_prefers_executables_over_batch_files() {
        let dir = std::env::temp_dir().join(format!("domo-path-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tool.exe");
        let cmd = dir.join("tool.cmd");
        std::fs::write(&exe, b"").unwrap();
        std::fs::write(&cmd, b"").unwrap();
        let path = Some(OsString::from(dir.as_os_str()));
        let pathext = Some(".COM;.EXE;.BAT;.CMD".to_string());

        let resolved = search_path_in("tool", path.clone(), pathext.clone()).unwrap();
        assert!(resolved
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("tool.exe"));
        std::fs::remove_file(&exe).unwrap();
        let resolved = search_path_in("tool", path, pathext).unwrap();
        assert!(resolved
            .to_string_lossy()
            .to_ascii_lowercase()
            .ends_with("tool.cmd"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn simple_batch_shim_resolves_to_wrapped_executable() {
        let dir = std::env::temp_dir().join(format!("domo-shim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("tool.exe");
        std::fs::write(&exe, b"").unwrap();
        let shim = dir.join("tool.cmd");
        std::fs::write(&shim, "\"%~dp0tool.exe\"   %*\r\n").unwrap();

        let resolved = resolve_program(&shim.to_string_lossy());
        assert_eq!(resolved.program, exe);
        assert!(resolved.prefix_args.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn single_line_branch_shim_uses_the_last_branch() {
        let dir = std::env::temp_dir().join(format!("domo-shim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let first = dir.join("first.exe");
        let last = dir.join("last.exe");
        std::fs::write(&first, b"").unwrap();
        std::fs::write(&last, b"").unwrap();
        let script = dir.join("cli.js");
        std::fs::write(&script, b"").unwrap();
        let shim = dir.join("tool.cmd");
        std::fs::write(
            &shim,
            "@IF EXIST \"%~dp0first.exe\" (\"%~dp0first.exe\" \"%~dp0cli.js\" %*) ELSE (\"%~dp0last.exe\" \"%~dp0cli.js\" %*)\r\n",
        )
        .unwrap();

        let resolved = resolve_program(&shim.to_string_lossy());
        assert_eq!(resolved.program, last);
        assert_eq!(resolved.prefix_args.len(), 1);
        assert!(resolved.prefix_args[0].ends_with("cli.js"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn npm_batch_shim_resolves_node_and_script() {
        let dir = std::env::temp_dir().join(format!("domo-shim-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let node = dir.join("node.exe");
        std::fs::write(&node, b"").unwrap();
        let script = dir.join("cli.js");
        std::fs::write(&script, b"").unwrap();
        let shim = dir.join("tool.cmd");
        std::fs::write(
            &shim,
            "@ECHO off\r\n\
             SETLOCAL\r\n\
             IF EXIST \"%dp0%\\node.exe\" (\r\n\
               SET \"_prog=%dp0%\\node.exe\"\r\n\
             ) ELSE (\r\n\
               SET \"_prog=node\"\r\n\
             )\r\n\
             endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\" \"%dp0%\\cli.js\" %*\r\n",
        )
        .unwrap();

        let resolved = resolve_program(&shim.to_string_lossy());
        assert_eq!(resolved.program, node);
        assert_eq!(resolved.prefix_args.len(), 1);
        assert!(resolved.prefix_args[0].ends_with("cli.js"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
