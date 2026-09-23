use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

pub(super) async fn git(cwd: &Path, args: &[&str]) -> Result<String> {
    let mut command = super::process::command("git", cwd);
    command.args([
        "-c",
        "core.hooksPath=NUL",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.longpaths=true",
    ]);
    command.args(args);
    let mut child = command.spawn().context("start managed Git operation")?;
    let job = super::process::ProcessJob::attach(&child)?;
    let stdout = tokio::spawn(super::process::drain(
        child.stdout.take().context("Git stdout missing")?,
    ));
    let stderr = tokio::spawn(super::process::drain(
        child.stderr.take().context("Git stderr missing")?,
    ));
    let status = match tokio::time::timeout(Duration::from_secs(120), child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            job.terminate()?;
            job.settle().await?;
            bail!("managed Git operation timed out");
        }
    };
    job.terminate()?;
    job.settle().await?;
    let (stdout, truncated) = stdout.await??;
    let (stderr, _) = stderr.await??;
    if !status.success() {
        bail!("Git operation failed: {}", String::from_utf8_lossy(&stderr));
    }
    if truncated {
        bail!("Git response exceeded output limit");
    }
    Ok(String::from_utf8(stdout)
        .context("Git response is not UTF-8")?
        .trim()
        .to_owned())
}

pub(super) async fn create_project(params: &Value) -> Result<Value> {
    let project = &params["project"];
    let id = project["id"].as_str().context("project.id required")?;
    Uuid::parse_str(id).context("project.id must be a UUID")?;
    let root = project["root"]
        .as_str()
        .context("project.root required")?
        .to_owned();
    let requested_root = root.clone();
    let (directory, _pins) = tokio::task::spawn_blocking(move || {
        let approved = super::super::project_directory::resolve(&root, true)?;
        if approved.as_os_str() != Path::new(&root).as_os_str() {
            bail!("project.root must be the pre-approved canonical new-project target");
        }
        super::super::project_directory::create_pinned(&root)
    })
    .await
    .context("new-project directory creation task failed; any partial directory remains for reconciliation")??;

    let initialized: Result<Value> = async {
        if directory.as_os_str() != Path::new(&requested_root).as_os_str() {
            bail!("new project target changed from its pre-approved canonical spelling");
        }
        git(
            &directory,
            &["init", "--initial-branch=main", "--template=", "."],
        )
        .await
        .context("initialize new project Git repository")?;
        git(
            &directory,
            &[
                "-c",
                "user.name=Agent Center",
                "-c",
                "user.email=agent-center@localhost",
                "commit",
                "--allow-empty",
                "--no-verify",
                "--no-gpg-sign",
                "-m",
                "Initialize Agent Center project",
            ],
        )
        .await
        .context("create initial empty project commit")?;
        let commit = git(&directory, &["rev-parse", "--verify", "HEAD^{commit}"])
            .await
            .context("verify initial project commit")?;
        if !matches!(commit.len(), 40 | 64) || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("new project Git repository returned an invalid initial commit ID");
        }
        Ok(json!({"projectId":id,"root":requested_root,"commitId":commit}))
    }
    .await;
    initialized.with_context(|| {
        format!(
            "new project initialization failed; partial directory {} remains for reconciliation",
            directory.display()
        )
    })
}

fn workspace_path(root: &Path, id: &str) -> Result<PathBuf> {
    Uuid::parse_str(id).context("managed workspace ID must be a UUID")?;
    Ok(root.join("workspaces").join(id))
}

async fn add_worktree(
    repository: &Path,
    path: &Path,
    fixed: &str,
    branch: Option<&str>,
) -> Result<()> {
    let path_arg = path.to_string_lossy();
    let mut args = vec!["worktree", "add", "--no-checkout"];
    if let Some(branch) = branch {
        args.extend(["-b", branch]);
    } else {
        args.push("--detach");
    }
    args.extend([path_arg.as_ref(), fixed]);
    git(repository, &args).await?;

    // Git for Windows' worktree checkout exports an absolute GIT_DIR, which
    // setup_explicit_git_dir rejects above PATH_MAX - 40 even with core.longpaths.
    // Run the same checkout from the new worktree instead, using Git discovery.
    // No path aliases, shared config changes, or user-checkout reset are needed.
    if let Err(error) = git(path, &["reset", "--hard", "--no-recurse-submodules", fixed]).await {
        let cleanup = git(repository, &["worktree", "remove", "--force", &path_arg]).await;
        if let Err(cleanup) = cleanup {
            bail!(
                "managed worktree checkout failed: {error:#}; removing incomplete worktree failed: {cleanup:#}"
            );
        }
        return Err(error).context("managed worktree checkout failed; incomplete worktree removed");
    }
    Ok(())
}

pub(super) async fn create(root: &Path, params: &Value) -> Result<Value> {
    let id = params["workspaceId"]
        .as_str()
        .context("workspaceId required")?;
    let path = workspace_path(root, id)?;
    if path.exists() {
        bail!("managed workspace already exists; reconcile the original create operation");
    }
    tokio::fs::create_dir_all(root.join("workspaces")).await?;
    let mut commit = None;
    let mut branch = None;
    let repository = params["repositoryPath"].as_str().or_else(|| {
        params["sourceRoot"].as_str().filter(|source| {
            params["kind"] == "LocalCode" || Path::new(source).join(".git").exists()
        })
    });
    if let Some(repository) = repository {
        let repository = PathBuf::from(repository).canonicalize()?;
        let base = params["commitId"].as_str().unwrap_or("HEAD");
        let fixed = git(
            &repository,
            &["rev-parse", "--verify", &format!("{base}^{{commit}}")],
        )
        .await?;
        if !fixed.chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("invalid resolved commit");
        }
        let name = format!("agent-center/{id}");
        add_worktree(&repository, &path, &fixed, Some(&name)).await?;
        branch = Some(name);
        commit = Some(fixed);
    } else {
        tokio::fs::create_dir_all(&path).await?;
    }
    Ok(
        json!({"workspaceId":id,"localPath":path,"localRoot":path,"root":path,
        "repositoryIdentity":repository,"repositoryPath":repository,"branch":branch,"commitId":commit}),
    )
}

pub(super) fn managed(root: &Path, workspace: &Path) -> Result<()> {
    let allowed = root
        .join("workspaces")
        .canonicalize()
        .context("managed workspaces unavailable")?;
    if !workspace.canonicalize()?.starts_with(allowed) {
        bail!("operation requires a managed workspace, not the user's worktree");
    }
    Ok(())
}

pub(super) async fn commit(root: &Path, workspace: &Path, message: &str) -> Result<Value> {
    managed(root, workspace)?;
    git(workspace, &["add", "--all"]).await?;
    let dirty = git(workspace, &["status", "--porcelain"]).await?;
    if !dirty.is_empty() {
        git(
            workspace,
            &[
                "-c",
                "user.name=Agent Center",
                "-c",
                "user.email=agent-center@localhost",
                "commit",
                "-m",
                message,
            ],
        )
        .await?;
    }
    let commit = git(workspace, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let branch = git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    Ok(json!({"localPath":workspace,"commitId":commit,"branch":branch}))
}

pub(super) async fn capture_commit(root: &Path, workspace: &Path, source: &Value) -> Result<Value> {
    if source.get("relativePath").is_some() {
        bail!("GitCommit forbids relativePath");
    }
    let requested = source["commitId"]
        .as_str()
        .context("GitCommit requires commitId")?;
    if requested.len() < 40 || !requested.chars().all(|c| c.is_ascii_hexdigit()) {
        bail!("GitCommit requires a full immutable commit ID, not a moving branch");
    }
    let fixed = git(
        workspace,
        &["rev-parse", "--verify", &format!("{requested}^{{commit}}")],
    )
    .await?;
    let checkout = root
        .join("staging")
        .join(format!("commit-{}", Uuid::new_v4()));
    tokio::fs::create_dir_all(root.join("staging")).await?;
    add_worktree(workspace, &checkout, &fixed, None).await?;
    let root_copy = root.to_owned();
    let checkout_copy = checkout.clone();
    let captured = tokio::task::spawn_blocking(move || {
        super::artifacts::capture(
            &root_copy,
            &checkout_copy,
            &json!({"kind":"Tree","relativePath":""}),
        )
    })
    .await?;
    let cleanup = git(
        workspace,
        &["worktree", "remove", "--force", &checkout.to_string_lossy()],
    )
    .await;
    let mut artifact = captured?;
    cleanup.context("remove managed commit capture checkout")?;
    // The content manifest remains the immutable Tree identity. Commit provenance
    // is outside that manifest, so identical trees retain independently verifiable bytes.
    artifact["kind"] = json!("GitCommit");
    artifact["commitId"] = json!(fixed);
    artifact["repositoryPath"] = json!(workspace);
    artifact["branch"] = json!(git(workspace, &["rev-parse", "--abbrev-ref", "HEAD"]).await?);
    Ok(artifact)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn new_project_initial_commit_supports_local_code_and_preserves_existing_paths() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("center-new-project-{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&root).await.unwrap();
        let source = root.canonicalize().unwrap().join("source");
        let id = Uuid::new_v4().to_string();
        assert!(
            create_project(&json!({"project":{"id":"not-a-uuid","root":source}}))
                .await
                .is_err()
        );
        assert!(!source.exists());
        assert!(
            create_project(&json!({"project":{"id":id,"root":root.join("not-approved")}}))
                .await
                .is_err()
        );
        assert!(!root.join("not-approved").exists());
        let params = json!({"project":{"id":id,"root":source}});
        let created = create_project(&params).await.unwrap();
        assert_eq!(created["projectId"], id);
        assert_eq!(created["root"], json!(source));
        assert_eq!(
            created["commitId"],
            git(&source, &["rev-parse", "--verify", "HEAD^{commit}"])
                .await
                .unwrap()
        );
        assert_eq!(
            git(&source, &["symbolic-ref", "--short", "HEAD"])
                .await
                .unwrap(),
            "main"
        );
        assert_eq!(
            git(&source, &["show", "-s", "--format=%an <%ae>"])
                .await
                .unwrap(),
            "Agent Center <agent-center@localhost>"
        );
        assert!(git(&source, &["ls-tree", "--name-only", "HEAD"])
            .await
            .unwrap()
            .is_empty());
        assert!(!source.join(".git").join("hooks").exists());
        let config = tokio::fs::read(source.join(".git").join("config"))
            .await
            .unwrap();
        tokio::fs::write(source.join("dirty.txt"), "uncommitted user work")
            .await
            .unwrap();
        let duplicate = create_project(&params).await.unwrap_err();
        assert!(duplicate.to_string().contains("already exists"));
        assert_eq!(
            tokio::fs::read_to_string(source.join("dirty.txt"))
                .await
                .unwrap(),
            "uncommitted user work"
        );
        assert_eq!(
            tokio::fs::read(source.join(".git").join("config"))
                .await
                .unwrap(),
            config
        );
        let existing_file = root.canonicalize().unwrap().join("existing.txt");
        tokio::fs::write(&existing_file, "existing file")
            .await
            .unwrap();
        assert!(
            create_project(&json!({"project":{"id":id,"root":existing_file}}))
                .await
                .is_err()
        );
        assert_eq!(
            tokio::fs::read_to_string(&existing_file).await.unwrap(),
            "existing file"
        );
        let managed_id = Uuid::new_v4().to_string();
        let workspace = create(
            &root,
            &json!({"workspaceId":managed_id,"sourceRoot":source,"kind":"LocalCode"}),
        )
        .await
        .unwrap();
        assert_eq!(workspace["commitId"], created["commitId"]);
        let managed_path = PathBuf::from(workspace["localRoot"].as_str().unwrap());
        assert_eq!(
            git(&managed_path, &["rev-parse", "--verify", "HEAD^{commit}"])
                .await
                .unwrap(),
            created["commitId"].as_str().unwrap()
        );
        assert!(!managed_path.join("dirty.txt").exists());
        git(
            &source,
            &[
                "worktree",
                "remove",
                "--force",
                &managed_path.to_string_lossy(),
            ],
        )
        .await
        .unwrap();
        writable(&root);
        tokio::fs::remove_dir_all(&root).await.unwrap();
    }

    #[tokio::test]
    async fn managed_worktree_and_fixed_capture_preserve_user_checkout() {
        let root = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("center-workspace-{}", Uuid::new_v4()));
        exercise_managed_worktree(&root, &root.join("source")).await;
    }

    #[cfg(windows)]
    fn long_root(base: &Path, workspace_length: usize) -> PathBuf {
        let root_length = workspace_length - "\\workspaces\\".len() - 36;
        assert!(base.as_os_str().len() + 1 < root_length);
        base.join("x".repeat(root_length - base.as_os_str().len() - 1))
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn long_managed_worktrees_and_capture_remain_usable() {
        for workspace_length in [237, 250] {
            let base = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("center-long-workspace-{}", Uuid::new_v4()));
            let root = long_root(&base, workspace_length);
            assert_eq!(
                workspace_path(&root, &Uuid::new_v4().to_string())
                    .unwrap()
                    .as_os_str()
                    .len(),
                workspace_length
            );
            exercise_managed_worktree(&root, &base.join("source")).await;
            writable(&base);
            tokio::fs::remove_dir_all(base).await.unwrap();
        }
    }

    async fn exercise_managed_worktree(root: &Path, source: &Path) {
        tokio::fs::create_dir_all(&source).await.unwrap();
        git(&source, &["init"]).await.unwrap();
        tokio::fs::write(source.join("code.txt"), "committed baseline")
            .await
            .unwrap();
        let nested_file = PathBuf::from("nested").join(format!("{}.txt", "tracked".repeat(8)));
        tokio::fs::create_dir_all(source.join("nested"))
            .await
            .unwrap();
        tokio::fs::write(source.join(&nested_file), "long tracked path")
            .await
            .unwrap();
        git(&source, &["add", "--all"]).await.unwrap();
        git(
            &source,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@localhost",
                "commit",
                "-m",
                "baseline",
            ],
        )
        .await
        .unwrap();
        let baseline = git(&source, &["rev-parse", "HEAD"]).await.unwrap();
        let original_config = tokio::fs::read(source.join(".git").join("config"))
            .await
            .unwrap();
        tokio::fs::write(source.join("code.txt"), "uncommitted user work")
            .await
            .unwrap();
        tokio::fs::write(source.join("staged.txt"), "staged user work")
            .await
            .unwrap();
        git(&source, &["add", "staged.txt"]).await.unwrap();
        tokio::fs::write(source.join("untracked.txt"), "untracked user work")
            .await
            .unwrap();
        let source_status = git(&source, &["status", "--porcelain"]).await.unwrap();
        let source_index = tokio::fs::read(source.join(".git").join("index"))
            .await
            .unwrap();
        let id = Uuid::new_v4().to_string();
        let receipt = create(
            root,
            &json!({"workspaceId":id,"sourceRoot":source,"kind":"LocalCode"}),
        )
        .await
        .unwrap();
        let managed_path = PathBuf::from(receipt["localRoot"].as_str().unwrap());
        assert_eq!(managed_path, workspace_path(root, &id).unwrap());
        assert_eq!(receipt["branch"], format!("agent-center/{id}"));
        assert!(!managed_path.join("staged.txt").exists());
        assert!(!managed_path.join("untracked.txt").exists());
        assert!(git(&managed_path, &["status", "--porcelain"])
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            tokio::fs::read_to_string(managed_path.join(&nested_file))
                .await
                .unwrap(),
            "long tracked path"
        );
        assert!(managed(root, &source).is_err());
        let duplicate = create(
            root,
            &json!({"workspaceId":id,"sourceRoot":source,"kind":"LocalCode"}),
        )
        .await
        .unwrap_err();
        assert!(duplicate.to_string().contains("already exists"));
        assert_eq!(
            tokio::fs::read_to_string(managed_path.join("code.txt"))
                .await
                .unwrap(),
            "committed baseline"
        );
        tokio::fs::write(managed_path.join("code.txt"), "fixed result")
            .await
            .unwrap();
        let committed = commit(root, &managed_path, "fixed output").await.unwrap();
        assert_ne!(committed["commitId"], baseline);
        let captured = capture_commit(
            root,
            &managed_path,
            &json!({"kind":"GitCommit","commitId":committed["commitId"]}),
        )
        .await
        .unwrap();
        let bytes = super::super::artifacts::verify(&captured).unwrap();
        assert_eq!(
            tokio::fs::read(source.join(".git").join("config"))
                .await
                .unwrap(),
            original_config
        );
        assert_eq!(
            tokio::fs::read_to_string(bytes.join("code.txt"))
                .await
                .unwrap(),
            "fixed result"
        );
        assert_eq!(
            tokio::fs::read_to_string(bytes.join(&nested_file))
                .await
                .unwrap(),
            "long tracked path"
        );
        assert_eq!(
            git(&source, &["rev-parse", "HEAD"]).await.unwrap(),
            baseline
        );
        assert_eq!(
            tokio::fs::read_to_string(source.join("code.txt"))
                .await
                .unwrap(),
            "uncommitted user work"
        );
        assert_eq!(
            tokio::fs::read(source.join(".git").join("index"))
                .await
                .unwrap(),
            source_index
        );
        assert_eq!(
            git(&source, &["status", "--porcelain"]).await.unwrap(),
            source_status
        );
        git(
            &source,
            &[
                "worktree",
                "remove",
                "--force",
                &managed_path.to_string_lossy(),
            ],
        )
        .await
        .unwrap();
        writable(root);
        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn long_worktree_checkout_failure_is_explicit_and_cleans_registration() {
        let base = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("center-failed-workspace-{}", Uuid::new_v4()));
        let root = long_root(&base, 237);
        let source = base.join("source");
        tokio::fs::create_dir_all(&source).await.unwrap();
        git(&source, &["init"]).await.unwrap();
        tokio::fs::write(source.join("code.txt"), "baseline")
            .await
            .unwrap();
        tokio::fs::write(
            source.join(".gitattributes"),
            "code.txt filter=center_checkout_failure\n",
        )
        .await
        .unwrap();
        git(&source, &["add", "--all"]).await.unwrap();
        git(
            &source,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@localhost",
                "commit",
                "-m",
                "baseline",
            ],
        )
        .await
        .unwrap();
        git(
            &source,
            &[
                "config",
                "filter.center_checkout_failure.smudge",
                "git --not-a-valid-option",
            ],
        )
        .await
        .unwrap();
        git(
            &source,
            &["config", "filter.center_checkout_failure.required", "true"],
        )
        .await
        .unwrap();
        let baseline = git(&source, &["rev-parse", "HEAD"]).await.unwrap();
        let original_config = tokio::fs::read(source.join(".git").join("config"))
            .await
            .unwrap();
        let id = Uuid::new_v4().to_string();
        let error = create(
            &root,
            &json!({"workspaceId":id,"sourceRoot":source,"kind":"LocalCode"}),
        )
        .await
        .unwrap_err();
        let detail = format!("{error:#}");
        assert!(
            detail.contains("managed worktree checkout failed"),
            "{detail}"
        );
        assert!(detail.contains("smudge filter"), "{detail}");
        assert!(detail.contains("incomplete worktree removed"), "{detail}");
        assert!(!workspace_path(&root, &id).unwrap().exists());
        assert_eq!(
            git(&source, &["worktree", "list", "--porcelain"])
                .await
                .unwrap()
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1
        );
        assert_eq!(
            git(&source, &["rev-parse", "HEAD"]).await.unwrap(),
            baseline
        );
        assert_eq!(
            tokio::fs::read(source.join(".git").join("config"))
                .await
                .unwrap(),
            original_config
        );
        assert_eq!(
            tokio::fs::read_to_string(source.join("code.txt"))
                .await
                .unwrap(),
            "baseline"
        );
        writable(&base);
        tokio::fs::remove_dir_all(base).await.unwrap();
    }

    fn writable(path: &Path) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                writable(&entry.path());
            } else {
                let mut permissions = entry.metadata().unwrap().permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                permissions.set_readonly(false);
                std::fs::set_permissions(entry.path(), permissions).unwrap();
            }
        }
    }
}
