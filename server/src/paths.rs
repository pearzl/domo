//! Project workspace layout. Directory names use a bounded prefix of an entity
//! id so repository checkouts stay within platform path limits.

use std::path::{Path, PathBuf};

/// 64 bits of an entity id. Large enough to avoid collisions between personal
/// workspaces while keeping every generated path much shorter than the id.
const DIR_KEY_LEN: usize = 16;

pub fn project_workspace(root: &Path, project_id: &str) -> PathBuf {
    root.join(prefixed_key(project_id))
}

pub fn project_repo(workspace: &Path) -> PathBuf {
    workspace.join("repo")
}

/// Job directories hold only flat run files, so they keep the full job id and
/// stay addressable without a lookup table.
pub fn job_dir(workspace: &Path, job_id: &str) -> PathBuf {
    workspace.join("jobs").join(job_id)
}

pub fn task_worktree(workspace: &Path, task_seq: i64) -> PathBuf {
    workspace.join("tasks").join(task_seq.to_string())
}

pub fn verify_worktree(workspace: &Path, task_seq: i64, job_id: &str) -> PathBuf {
    workspace
        .join("verify")
        .join(format!("{task_seq}-{}", dir_key(job_id)))
}

fn dir_key(id: &str) -> String {
    let key = id.rsplit_once('_').map(|(_, suffix)| suffix).unwrap_or(id);
    key.chars().take(DIR_KEY_LEN).collect()
}

fn prefixed_key(id: &str) -> String {
    match id.rsplit_once('_') {
        Some((prefix, suffix)) => format!(
            "{prefix}_{}",
            suffix.chars().take(DIR_KEY_LEN).collect::<String>()
        ),
        None => dir_key(id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_keep_the_project_prefix_and_bound_the_length() {
        assert_eq!(
            prefixed_key("prj_0123456789abcdef0123456789abcdef"),
            "prj_0123456789abcdef"
        );
        assert_eq!(
            dir_key("job_0123456789abcdef0123456789abcdef"),
            "0123456789abcdef"
        );
        assert_eq!(prefixed_key("short"), "short");
    }

    #[test]
    fn worktree_paths_use_short_directory_names() {
        let root = Path::new("/home/domo/.domo/workspace");
        let workspace = project_workspace(root, "prj_0123456789abcdef0123456789abcdef");
        assert_eq!(workspace, root.join("prj_0123456789abcdef"));
        assert_eq!(task_worktree(&workspace, 128), workspace.join("tasks/128"));
        assert_eq!(
            verify_worktree(&workspace, 128, "job_0123456789abcdef0123456789abcdef"),
            workspace.join("verify/128-0123456789abcdef")
        );
    }
}
