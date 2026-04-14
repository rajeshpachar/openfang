// US-003: Branch management -- per-issue branches off agent/dev + git worktrees.
// Phase 1 skeleton: cleanup_issue and current_branch used in Phase 2.
#![allow(dead_code)]
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::{Path, PathBuf};
use std::process::Command;

const AGENT_DEV_BRANCH: &str = "agent/dev";

/// Run a git command in `cwd` and return trimmed stdout.
/// Fails with a descriptive error if the command returns non-zero.
fn git(args: &[&str], cwd: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to run: git {}", args.join(" ")))?;

    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        bail!(
            "`git {}` failed in {}:\n{}",
            args.join(" "),
            cwd.display(),
            stderr
        )
    }
}

/// Check if the working tree has uncommitted changes.
pub fn has_uncommitted_changes(repo_root: &Path) -> Result<bool> {
    let out = git(&["status", "--porcelain"], repo_root)?;
    Ok(!out.is_empty())
}

/// Return the current branch name.
pub fn current_branch(repo_root: &Path) -> Result<String> {
    git(&["rev-parse", "--abbrev-ref", "HEAD"], repo_root)
}

/// Return the short commit hash at HEAD in the given directory.
pub fn head_commit(dir: &Path) -> Result<String> {
    git(&["rev-parse", "--short", "HEAD"], dir)
}

/// Return true if a local branch exists.
pub fn branch_exists(repo_root: &Path, branch: &str) -> Result<bool> {
    let out = Command::new("git")
        .args(["branch", "--list", branch])
        .current_dir(repo_root)
        .output()
        .context("Failed to run git branch --list")?;
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

/// Ensure `agent/dev` exists (branched from `base_branch`) and is synced.
///
/// Steps:
///   1. If `agent/dev` does not exist: create it from `base_branch`.
///   2. Checkout `agent/dev`.
///   3. Merge `base_branch` (fast-forward preferred; fail on conflict).
pub fn ensure_agent_dev(repo_root: &Path, base_branch: &str) -> Result<()> {
    if !branch_exists(repo_root, AGENT_DEV_BRANCH)? {
        // Create agent/dev from base_branch
        git(
            &["checkout", "-b", AGENT_DEV_BRANCH, base_branch],
            repo_root,
        )?;
        println!(
            "  {} Created {} from {}",
            "✓".green(),
            AGENT_DEV_BRANCH.cyan(),
            base_branch
        );
    } else {
        // Checkout and sync
        git(&["checkout", AGENT_DEV_BRANCH], repo_root)?;

        let merge_result = Command::new("git")
            .args(["merge", base_branch, "--no-edit", "--ff-only"])
            .current_dir(repo_root)
            .output()
            .context("Failed to run git merge")?;

        if !merge_result.status.success() {
            // Try regular merge (non-fast-forward)
            let regular = Command::new("git")
                .args(["merge", base_branch, "--no-edit"])
                .current_dir(repo_root)
                .output()
                .context("Failed to run git merge")?;

            if !regular.status.success() {
                let stderr = String::from_utf8_lossy(&regular.stderr);
                bail!(
                    "Cannot sync {} with {} — merge conflict.\n{}\nResolve manually, then run `pipeline resume {{issueKey}}`.",
                    AGENT_DEV_BRANCH,
                    base_branch,
                    stderr.trim()
                );
            }
            println!(
                "  {} Synced {} with {} (merge commit)",
                "✓".green(),
                AGENT_DEV_BRANCH.cyan(),
                base_branch
            );
        } else {
            let ahead = git(
                &["rev-list", "--count", &format!("{}..HEAD", base_branch)],
                repo_root,
            )
            .unwrap_or_default();
            let label = if ahead == "0" {
                "already up to date".to_string()
            } else {
                format!("+{} commits", ahead)
            };
            println!(
                "  {} Synced {} with {} ({})",
                "✓".green(),
                AGENT_DEV_BRANCH.cyan(),
                base_branch,
                label
            );
        }
    }
    Ok(())
}

/// Create the per-issue branch `pipeline/{issue_key}` from `agent/dev`.
/// Returns the branch name.
pub fn create_issue_branch(repo_root: &Path, issue_key: &str) -> Result<String> {
    let branch = format!("pipeline/{}", issue_key);

    if branch_exists(repo_root, &branch)? {
        // Resume — branch is already checked out in its worktree.
        // Do NOT try to check it out in the main repo: git refuses when a branch
        // is already used by another worktree.
        println!(
            "  {} Resuming existing branch {}",
            "↩".yellow(),
            branch.cyan()
        );
    } else {
        // New issue — create branch from agent/dev (checks it out in main repo).
        // setup_issue_workspace will switch main back to base_branch immediately after.
        git(
            &["checkout", "-b", &branch, AGENT_DEV_BRANCH],
            repo_root,
        )?;
        println!("  {} Created {} from {}", "✓".green(), branch.cyan(), AGENT_DEV_BRANCH);
    }

    Ok(branch)
}

/// Default worktree base directory.
pub fn default_worktree_base() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".pipeline")
        .join("worktrees")
}

/// Create (or reuse) a git worktree for the issue at `~/.pipeline/worktrees/{issueKey}/`.
///
/// Returns the absolute worktree path.
pub fn ensure_worktree(
    repo_root: &Path,
    issue_key: &str,
    branch: &str,
) -> Result<PathBuf> {
    let worktree_path = default_worktree_base().join(issue_key);

    if worktree_path.exists() {
        println!(
            "  {} Worktree exists: {}",
            "↩".yellow(),
            worktree_path.display()
        );
        return Ok(worktree_path);
    }

    std::fs::create_dir_all(&worktree_path)
        .with_context(|| format!("Failed to create worktree dir {}", worktree_path.display()))?;

    // Remove the empty dir so git worktree add can create it
    std::fs::remove_dir(&worktree_path)
        .with_context(|| format!("Failed to remove placeholder dir {}", worktree_path.display()))?;

    git(
        &[
            "worktree",
            "add",
            &worktree_path.to_string_lossy(),
            branch,
        ],
        repo_root,
    )?;

    println!(
        "  {} Worktree: {}",
        "✓".green(),
        worktree_path.display()
    );

    Ok(worktree_path)
}

/// Remove worktree and delete the issue branch after PR merge.
pub fn cleanup_issue(
    repo_root: &Path,
    issue_key: &str,
    branch: &str,
    base_branch: &str,
) -> Result<()> {
    let worktree_path = default_worktree_base().join(issue_key);

    if worktree_path.exists() {
        git(
            &[
                "worktree",
                "remove",
                "--force",
                &worktree_path.to_string_lossy(),
            ],
            repo_root,
        )?;
        println!("  {} Worktree removed", "✓".green());
    }

    // Switch away from the branch before deleting
    git(&["checkout", base_branch], repo_root)?;

    if branch_exists(repo_root, branch)? {
        git(&["branch", "-D", branch], repo_root)?;
        println!("  {} Branch {} deleted", "✓".green(), branch.cyan());
    }

    Ok(())
}

/// Full setup for a new issue: ensure agent/dev → create branch → create worktree.
/// Returns `(branch_name, worktree_path)`.
pub fn setup_issue_workspace(
    repo_root: &Path,
    issue_key: &str,
    base_branch: &str,
) -> Result<(String, PathBuf)> {
    if has_uncommitted_changes(repo_root)? {
        bail!(
            "Working tree has uncommitted changes. Commit or stash them before starting the pipeline."
        );
    }

    ensure_agent_dev(repo_root, base_branch)?;
    let branch = create_issue_branch(repo_root, issue_key)?;

    // Switch the main worktree back to base_branch so the issue branch can be
    // added as a separate worktree. Git refuses to add a worktree for a branch
    // that is already checked out in the main worktree.
    git(&["checkout", base_branch], repo_root)?;

    let worktree_path = ensure_worktree(repo_root, issue_key, &branch)?;

    Ok((branch, worktree_path))
}

// ---------------------------------------------------------------------------
// US-014: Story revert — git reset + checkout + clean
// ---------------------------------------------------------------------------

/// Classify files changed in the last commit as added (new) vs modified/deleted.
/// Returns `(modified_or_deleted, added)`.
/// Uses `git diff --name-status HEAD~1 HEAD`.
pub fn classify_last_commit_files(worktree: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let out = git(&["diff", "--name-status", "HEAD~1", "HEAD"], worktree)?;
    let mut modified = Vec::new();
    let mut added = Vec::new();

    for line in out.lines() {
        if let Some((status, file)) = line.split_once('\t') {
            if status.starts_with('A') {
                added.push(file.to_string());
            } else {
                // M = modified, D = deleted, R = renamed, C = copied
                modified.push(file.to_string());
            }
        }
    }

    Ok((modified, added))
}

/// Revert the last commit and restore the working tree to its pre-story state.
///
/// Steps (scoped to story files only — never touches the full working tree):
///   1. `git reset HEAD~1 --mixed`    — moves HEAD back, unstages changes
///   2. `git checkout -- {modified}`  — restores tracked files to HEAD (now parent)
///   3. `git clean -fd -- {added}`    — removes newly created (untracked) files
pub fn revert_story_commit(worktree: &Path) -> Result<()> {
    let (modified, added) = classify_last_commit_files(worktree)?;

    // Step 1: move HEAD back (working tree unchanged after this)
    git(&["reset", "HEAD~1", "--mixed"], worktree)?;

    // Step 2: restore modified/deleted tracked files to the parent commit
    if !modified.is_empty() {
        let mut args: Vec<&str> = vec!["checkout", "--"];
        let refs: Vec<&str> = modified.iter().map(|s| s.as_str()).collect();
        args.extend(refs.iter().copied());
        git(&args, worktree)?;
    }

    // Step 3: remove added files (now untracked after reset) — scoped per-file
    for file in &added {
        // Ignore errors: file might have already been cleaned up
        let _ = git(&["clean", "-fd", "--", file], worktree);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        Command::new("git").args(["init"]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["config", "user.email", "test@test.com"]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["config", "user.name", "Test"]).current_dir(dir.path()).output().unwrap();
        // Create initial commit on main
        std::fs::write(dir.path().join("README.md"), "# test").unwrap();
        Command::new("git").args(["add", "."]).current_dir(dir.path()).output().unwrap();
        Command::new("git").args(["commit", "-m", "init"]).current_dir(dir.path()).output().unwrap();
        // Rename to main if needed
        let _ = Command::new("git").args(["branch", "-M", "main"]).current_dir(dir.path()).output();
        dir
    }

    #[test]
    fn test_branch_exists_false() {
        let repo = init_repo();
        assert!(!branch_exists(repo.path(), "agent/dev").unwrap());
    }

    #[test]
    fn test_ensure_agent_dev_creates_branch() {
        let repo = init_repo();
        ensure_agent_dev(repo.path(), "main").unwrap();
        assert!(branch_exists(repo.path(), "agent/dev").unwrap());
    }

    #[test]
    fn test_create_issue_branch() {
        let repo = init_repo();
        ensure_agent_dev(repo.path(), "main").unwrap();
        let branch = create_issue_branch(repo.path(), "TEST-001").unwrap();
        assert_eq!(branch, "pipeline/TEST-001");
        assert!(branch_exists(repo.path(), "pipeline/TEST-001").unwrap());
    }

    #[test]
    fn test_has_uncommitted_changes_clean() {
        let repo = init_repo();
        assert!(!has_uncommitted_changes(repo.path()).unwrap());
    }

    #[test]
    fn test_has_uncommitted_changes_dirty() {
        let repo = init_repo();
        std::fs::write(repo.path().join("new_file.txt"), "dirty").unwrap();
        assert!(has_uncommitted_changes(repo.path()).unwrap());
    }
}
