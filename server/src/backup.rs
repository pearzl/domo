//! Offline snapshots and verification. An OS-held lease excludes the server
//! and concurrent backups; checksums cover files, symlinks and Git archives.
use crate::{config::Config, git, models::ProjectRow};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs::{File, OpenOptions},
    io::Read,
    path::{Path, PathBuf},
};

pub fn lease(root: &Path) -> Result<File> {
    std::fs::create_dir_all(root)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("runtime.lock"))?;
    file.try_lock()
        .context("数据目录正在被服务或其他备份进程使用，请先停止服务")?;
    Ok(file)
}

#[derive(Serialize, Deserialize)]
struct Root {
    original: PathBuf,
    relative: PathBuf,
}
#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    workspace: PathBuf,
    roots: Vec<Root>,
    worktrees: Vec<PathBuf>,
    git_admin: Vec<PathBuf>,
    hashes: HashMap<String, String>,
}

fn hash(path: &Path) -> Result<String> {
    let mut value = Sha256::new();
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        value.update(b"symlink:");
        value.update(std::fs::read_link(path)?.to_string_lossy().as_bytes());
    } else {
        let mut input = File::open(path)?;
        let mut bytes = [0_u8; 65536];
        loop {
            let n = input.read(&mut bytes)?;
            if n == 0 {
                break;
            }
            value.update(&bytes[..n]);
        }
    }
    Ok(format!("{:x}", value.finalize()))
}

fn copy_tree(source: &Path, target: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let link = std::fs::read_link(source)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(link, target)?;
        #[cfg(windows)]
        {
            if source.is_dir() {
                std::os::windows::fs::symlink_dir(link, target)?;
            } else {
                std::os::windows::fs::symlink_file(link, target)?;
            }
        }
    } else if metadata.is_dir() {
        std::fs::create_dir_all(target)?;
        for entry in std::fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
    } else {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(source, target)?;
        if hash(source)? != hash(target)? {
            bail!("备份期间源文件变化：{}", source.display());
        }
    }
    Ok(())
}

fn checksums(root: &Path) -> Result<HashMap<String, String>> {
    let mut result = HashMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                let path = entry.path();
                let key = path
                    .strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/");
                if key != "manifest.json" {
                    result.insert(key, hash(&path)?);
                }
            }
        }
    }
    Ok(result)
}

fn mapped(path: &Path, manifest: &Manifest, root: &Path) -> Result<PathBuf> {
    let best = manifest
        .roots
        .iter()
        .filter(|entry| path.starts_with(&entry.original))
        .max_by_key(|entry| entry.original.components().count());
    match best {
        Some(entry) => Ok(root
            .join(&entry.relative)
            .join(path.strip_prefix(&entry.original)?)),
        None => bail!("备份缺少路径映射：{}", path.display()),
    }
}

fn add_root(roots: &mut Vec<Root>, source: PathBuf, relative: PathBuf) {
    if !roots.iter().any(|r| source.starts_with(&r.original)) {
        roots.push(Root {
            original: source,
            relative,
        });
    }
}

pub async fn create(cfg: &Config, destination: &Path) -> Result<()> {
    let _lease = lease(&cfg.data_root)?;
    let pool = crate::db::connect(&cfg.db_path()).await?;
    let running: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs WHERE status = 'running'")
        .fetch_one(&pool)
        .await?;
    if running != 0 {
        bail!("存在中断后尚未结算的作业，请启动服务完成恢复并暂停项目后再备份");
    }
    let projects = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects")
        .fetch_all(&pool)
        .await?;
    let mut roots = Vec::new();
    let workspace = std::fs::canonicalize(&cfg.workspace_root)?;
    add_root(&mut roots, workspace.clone(), "trees/workspace".into());
    let mut worktrees = Vec::new();
    let mut git_admin = Vec::new();
    for (index, project) in projects.iter().enumerate() {
        let source = std::fs::canonicalize(&project.repo_path)
            .with_context(|| format!("项目产物目录不可读：{}", project.repo_path))?;
        add_root(
            &mut roots,
            source.clone(),
            format!("trees/project-{index}").into(),
        );
        if project.is_git() {
            let admin = git::run_git(
                &source,
                &["rev-parse", "--path-format=absolute", "--git-common-dir"],
            )
            .await?;
            let admin = std::fs::canonicalize(admin.trim())?;
            add_root(
                &mut roots,
                admin.clone(),
                format!("trees/git-admin-{index}").into(),
            );
            if !git_admin.contains(&admin) {
                git_admin.push(admin);
            }
            let entries = git::run_git(&source, &["worktree", "list", "--porcelain"]).await?;
            for line in entries
                .lines()
                .filter_map(|line| line.strip_prefix("worktree "))
            {
                let path = PathBuf::from(line);
                if path.exists() {
                    let path = std::fs::canonicalize(path)?;
                    add_root(
                        &mut roots,
                        path.clone(),
                        format!("trees/worktree-{}", worktrees.len()).into(),
                    );
                    if !worktrees.contains(&path) {
                        worktrees.push(path);
                    }
                }
            }
        }
    }
    let absolute = if destination.is_absolute() {
        destination.to_path_buf()
    } else {
        std::env::current_dir()?.join(destination)
    };
    for source in &roots {
        if absolute.starts_with(&source.original) || source.original.starts_with(&absolute) {
            bail!("备份目录不能与源数据目录重叠");
        }
    }
    if absolute.starts_with(&cfg.data_root) {
        bail!("备份目录不能位于数据目录内");
    }
    std::fs::create_dir(&absolute).context("备份目标必须是不存在的目录，不覆盖已有文件")?;
    std::fs::create_dir(absolute.join("data"))?;
    let database = absolute.join("data/domo.db");
    sqlx::query("VACUUM INTO ?")
        .bind(database.to_string_lossy().as_ref())
        .execute(&pool)
        .await?;
    for source in &roots {
        copy_tree(&source.original, &absolute.join(&source.relative))?;
    }
    let mut manifest = Manifest {
        version: 1,
        workspace,
        roots,
        worktrees,
        git_admin,
        hashes: HashMap::new(),
    };
    manifest.hashes = checksums(&absolute)?;
    std::fs::write(
        absolute.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;
    pool.close().await;
    verify(&absolute).await?;
    println!(
        "备份已完成并校验：{}（{} 个文件/链接，含数据库、共享仓库、作业、普通产物及全部 Git 引用）",
        absolute.display(),
        manifest.hashes.len()
    );
    Ok(())
}

pub async fn verify(source: &Path) -> Result<()> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(source.join("manifest.json"))?)?;
    if manifest.version != 1 {
        bail!("不支持的备份版本");
    }
    if checksums(source)? != manifest.hashes {
        bail!("备份校验失败：存在缺失、额外或内容变化的文件");
    }
    for path in manifest.hashes.keys() {
        backup_relative(Path::new(path))?;
    }
    for entry in &manifest.roots {
        backup_relative(&entry.relative)?;
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(source.join("data/domo.db"))
        .read_only(true);
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    let integrity: String = sqlx::query_scalar("PRAGMA integrity_check")
        .fetch_one(&pool)
        .await?;
    if integrity != "ok" {
        bail!("数据库完整性检查失败：{integrity}");
    }
    let broken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM jobs j LEFT JOIN projects p ON p.id = j.project_id LEFT JOIN contracts c ON c.id = j.contract_id WHERE p.id IS NULL OR c.id IS NULL")
        .fetch_one(&pool).await?;
    if broken != 0 {
        bail!("作业与项目/契约关联不完整：{broken}");
    }
    let projects = sqlx::query_as::<_, ProjectRow>("SELECT * FROM projects")
        .fetch_all(&pool)
        .await?;
    for project in &projects {
        let directory = mapped(Path::new(&project.workspace), &manifest, source)?;
        if !directory.join("shared/.git").is_dir() {
            bail!("缺少项目共享信息仓库：{}", project.id);
        }
        let jobs: Vec<String> = sqlx::query_scalar(
            "SELECT id FROM jobs WHERE project_id = ? AND started_at IS NOT NULL",
        )
        .bind(&project.id)
        .fetch_all(&pool)
        .await?;
        for job in jobs {
            if !directory.join("jobs").join(job).is_dir() {
                bail!("缺少作业审计目录");
            }
        }
        if !mapped(Path::new(&project.repo_path), &manifest, source)?.is_dir() {
            bail!("普通产物或仓库路径不可访问");
        }
    }
    for original in &manifest.git_admin {
        let admin = mapped(original, &manifest, source)?;
        git::run_git(&admin, &["--git-dir=.", "fsck", "--full"]).await?;
    }
    pool.close().await;
    println!(
        "备份校验通过：校验和、数据库关联、Git 对象与归档引用、共享仓库、作业目录及产物可访问"
    );
    Ok(())
}

fn backup_relative(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        bail!("备份清单包含非法相对路径");
    }
    Ok(())
}

pub async fn cli(cfg: &Config) -> Result<bool> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--backup") if args.len() == 2 => {
            create(cfg, Path::new(&args[1])).await?;
            Ok(true)
        }
        Some("--verify-backup") if args.len() == 2 => {
            verify(Path::new(&args[1])).await?;
            Ok(true)
        }
        Some(_) => bail!("用法：domo-server [--backup <新目录> | --verify-backup <备份目录>]"),
        None => Ok(false),
    }
}
