// US-005: Prompt assembly engine.
// Phase 1 skeleton: assemble, update_progress, and supporting types used in Phase 2.
#![allow(dead_code)]
///
/// Assembles the full prompt for a Claude CLI call in the correct order:
///   1. CLAUDE.md (repo conventions)
///   2. PIPELINE/PROGRESS.md (if exists — mandatory after first story)
///   3. Role standards file (backend.md or frontend.md)
///   4. Backlog issue JSON
///   5. Phase-specific instruction block
///   6. Repo map (optional — only if repo_map_lines > 0)
///
/// Writes prompt to PIPELINE/PROMPT-{key}-{phase}.md for audit.
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

/// Phase identifier — determines which instruction block is appended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPhase {
    Decompose,
    Execute,
    GapFix,
    Pr,
}

impl PromptPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decompose => "decompose",
            Self::Execute => "execute",
            Self::GapFix => "gapfix",
            Self::Pr => "pr",
        }
    }
}

/// Backlog issue data passed into prompt assembly.
#[derive(Debug, Clone)]
pub struct IssueContext {
    pub key: String,
    pub summary: String,
    pub description: String,
    pub issue_type: String,
    pub priority: String,
}

/// Story context for Execute/GapFix phases.
#[derive(Debug, Clone)]
pub struct StoryContext {
    pub id: String,
    pub title: String,
    pub completed_stories: Vec<String>,
    pub session_note: String,
    pub feedback: Option<String>, // For GapFix phase only
}

/// Assembled prompt with metadata.
#[derive(Debug)]
pub struct AssembledPrompt {
    pub text: String,
    pub estimated_tokens: usize,
    pub sections: PromptSections,
}

#[derive(Debug)]
pub struct PromptSections {
    pub claude_md_tokens: usize,
    pub progress_tokens: usize,
    pub role_tokens: usize,
    pub issue_tokens: usize,
    pub phase_tokens: usize,
    pub repo_map_tokens: usize,
}

/// Estimate token count — rough approximation: 1 token ≈ 4 chars.
fn estimate_tokens(text: &str) -> usize {
    text.len() / 4
}

/// Build the prompt for `phase` and write it to `PIPELINE/PROMPT-{key}-{phase}.md`.
pub fn assemble(
    repo_root: &Path,
    issue: &IssueContext,
    role_file: &Path,
    phase: PromptPhase,
    story_ctx: Option<&StoryContext>,
    repo_map_lines: u32,
) -> Result<AssembledPrompt> {
    // 1. CLAUDE.md
    let claude_md_path = repo_root.join("CLAUDE.md");
    if !claude_md_path.exists() {
        bail!(
            "No CLAUDE.md found at {}.\nRun `pipeline setup` first.",
            claude_md_path.display()
        );
    }
    let claude_md = std::fs::read_to_string(&claude_md_path)
        .with_context(|| format!("Failed to read {}", claude_md_path.display()))?;
    let claude_md_tokens = estimate_tokens(&claude_md);

    // 2. PIPELINE/PROGRESS.md (mandatory after first story)
    let progress_path = repo_root.join("PIPELINE").join("PROGRESS.md");
    let progress_text = if progress_path.exists() {
        std::fs::read_to_string(&progress_path)
            .with_context(|| format!("Failed to read {}", progress_path.display()))?
    } else {
        String::new()
    };
    let progress_tokens = estimate_tokens(&progress_text);

    // 3. Role standards file
    if !role_file.exists() {
        bail!(
            "Role standards file not found: {}.\nRun `pipeline setup` to generate it.",
            role_file.display()
        );
    }
    let role_text = std::fs::read_to_string(role_file)
        .with_context(|| format!("Failed to read {}", role_file.display()))?;
    let role_tokens = estimate_tokens(&role_text);

    // 4. Issue JSON block
    let issue_block = format!(
        "---\n\nBACKLOG ISSUE: {}\nSummary: {}\nType: {}\nPriority: {}\n\nDescription:\n{}\n",
        issue.key, issue.summary, issue.issue_type, issue.priority, issue.description
    );
    let issue_tokens = estimate_tokens(&issue_block);

    // 5. Phase instruction block
    let phase_block = build_phase_block(phase, issue, story_ctx);
    let phase_tokens = estimate_tokens(&phase_block);

    // 6. Repo map (opt-in)
    let (repo_map_text, repo_map_tokens) = if repo_map_lines > 0 {
        let map = generate_repo_map(repo_root, repo_map_lines)?;
        let t = estimate_tokens(&map);
        (map, t)
    } else {
        (String::new(), 0)
    };

    // Assemble in order
    let mut parts: Vec<&str> = vec![claude_md.as_str()];
    if !progress_text.is_empty() {
        parts.push(progress_text.as_str());
    }
    parts.push(role_text.as_str());
    parts.push(issue_block.as_str());
    parts.push(phase_block.as_str());
    if !repo_map_text.is_empty() {
        parts.push(repo_map_text.as_str());
    }

    let full_prompt = parts.join("\n\n---\n\n");
    let estimated_tokens = claude_md_tokens
        + progress_tokens
        + role_tokens
        + issue_tokens
        + phase_tokens
        + repo_map_tokens;

    // Write to PIPELINE/PROMPT-{key}-{phase}.md for audit
    let prompt_dir = repo_root.join("PIPELINE");
    std::fs::create_dir_all(&prompt_dir)
        .with_context(|| format!("Failed to create {}", prompt_dir.display()))?;
    let prompt_file = prompt_dir.join(format!("PROMPT-{}-{}.md", issue.key, phase.as_str()));
    std::fs::write(&prompt_file, &full_prompt)
        .with_context(|| format!("Failed to write {}", prompt_file.display()))?;

    Ok(AssembledPrompt {
        text: full_prompt,
        estimated_tokens,
        sections: PromptSections {
            claude_md_tokens,
            progress_tokens,
            role_tokens,
            issue_tokens,
            phase_tokens,
            repo_map_tokens,
        },
    })
}

fn build_phase_block(
    phase: PromptPhase,
    issue: &IssueContext,
    story_ctx: Option<&StoryContext>,
) -> String {
    match phase {
        PromptPhase::Decompose => format!(
            r#"PHASE: PLAN

Read the issue above. Explore the codebase to understand scope.
Break this into user stories if it is large (> one concern).
Keep each story to 15-30 minutes of focused work.
Write the plan to PIPELINE/PLAN-{key}.md.

Story format:
### US-001: {{verb + outcome}}
**Depends on:** none | US-00X
**Description:** one sentence
**Acceptance Criteria:**
- [ ] testable criterion
**Test scope:** exact command (e.g. cargo test -p crate-name filter)

Then output ONLY this JSON (no other text after the JSON):
{{"stories": [{{"id":"US-001","title":"...","depends_on":[]}}], "session_note": "key context for subsequent stories"}}"#,
            key = issue.key
        ),

        PromptPhase::Execute => {
            let ctx = story_ctx.expect("StoryContext required for Execute phase");
            let completed = if ctx.completed_stories.is_empty() {
                "none".to_string()
            } else {
                ctx.completed_stories.join(", ")
            };
            format!(
                r#"PHASE: IMPLEMENT {story_id}

The plan is at PIPELINE/PLAN-{issue_key}.md.
Completed stories: {completed}
Session context: {session_note}

Implement {story_id} only. Do not touch other stories.
Run only the test scope defined for this story in the plan.
Fix test failures before reporting completion.
Commit with message: "{issue_key} {story_id}: {story_title}"

Then output ONLY this JSON:
{{"story_id":"{story_id}","status":"done|blocked","blocker":null,"files_changed":[],"tests_run":[],"test_passed":true,"failure_type":"none|test_failure|compilation_error|timeout|budget_exhausted|unknown","handoff_note":"key context","progress_update":"new pattern for PROGRESS.md"}}"#,
                story_id = ctx.id,
                issue_key = issue.key,
                completed = completed,
                session_note = ctx.session_note,
                story_title = ctx.title,
            )
        }

        PromptPhase::GapFix => {
            let ctx = story_ctx.expect("StoryContext required for GapFix phase");
            let feedback = ctx.feedback.as_deref().unwrap_or("No specific feedback provided.");
            format!(
                r#"PHASE: FIX {story_id}

Reviewer feedback:
{feedback}

Address this feedback in the current implementation.
Re-run the story's test scope from PIPELINE/PLAN-{issue_key}.md.
Amend the commit for {story_id}.

Then output ONLY this JSON:
{{"story_id":"{story_id}","status":"done|blocked","blocker":null,"files_changed":[],"tests_run":[],"test_passed":true,"failure_type":"none|test_failure|compilation_error|timeout|budget_exhausted|unknown","handoff_note":"key context","progress_update":"new pattern for PROGRESS.md"}}"#,
                story_id = ctx.id,
                issue_key = issue.key,
                feedback = feedback,
            )
        }

        PromptPhase::Pr => {
            let ctx = story_ctx.expect("StoryContext required for PR phase");
            let completed = ctx.completed_stories.join(", ");
            format!(
                r#"PHASE: OPEN PR

All stories complete. Completed: {completed}
Branch: pipeline/{issue_key}

Verify all story commits are on this branch.
Push the branch to the fork remote.
Open a draft PR using gh pr create:
  --draft
  --base agent/dev
  --title "{issue_key}: {issue_summary}"
  --body "Closes Backlog issue {issue_key}. Stories: {completed}. See PIPELINE/PLAN-{issue_key}.md for details."

Then output ONLY this JSON:
{{"pr_url":"...","commits":[]}}"#,
                completed = completed,
                issue_key = issue.key,
                issue_summary = issue.summary,
            )
        }
    }
}

/// Generate a repo map: public symbols from Rust source files, capped at `max_lines`.
fn generate_repo_map(repo_root: &Path, max_lines: u32) -> Result<String> {
    let out = Command::new("rg")
        .args([
            r"(^|\s)pub (fn|struct|trait|enum|type)",
            "crates/",
            "-g",
            "*.rs",
            "-n",
            "--no-heading",
        ])
        .current_dir(repo_root)
        .output();

    match out {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = text.lines().take(max_lines as usize).collect();
            if lines.is_empty() {
                return Ok(String::new());
            }
            Ok(format!(
                "REPO MAP (top {} public symbols):\n```\n{}\n```",
                max_lines,
                lines.join("\n")
            ))
        }
        _ => Ok(String::new()), // rg not available or crates/ not found — skip silently
    }
}

/// Update `PIPELINE/PROGRESS.md` by appending a new `progress_update` string.
/// Called after each story completes.
pub fn update_progress(repo_root: &Path, issue_key: &str, story_id: &str, update: &str) -> Result<()> {
    let path = repo_root.join("PIPELINE").join("PROGRESS.md");
    std::fs::create_dir_all(path.parent().unwrap())?;

    let existing = if path.exists() {
        std::fs::read_to_string(&path).unwrap_or_default()
    } else {
        format!("# Codebase Progress Notes\n\nIssue: {}\n\n", issue_key)
    };

    let updated = format!("{}\n## {} — {}\n{}\n", existing, issue_key, story_id, update);
    std::fs::write(&path, updated)
        .with_context(|| format!("Failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_repo() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Test Repo\nConventions here.").unwrap();
        std::fs::create_dir_all(dir.path().join(".pipeline")).unwrap();
        std::fs::write(
            dir.path().join(".pipeline").join("backend.md"),
            "# Backend Standards\nUse Rust.",
        )
        .unwrap();
        dir
    }

    fn sample_issue() -> IssueContext {
        IssueContext {
            key: "TEST-001".into(),
            summary: "Add hello world function".into(),
            description: "Add a hello_world() function that returns a greeting string.".into(),
            issue_type: "Task".into(),
            priority: "Normal".into(),
        }
    }

    #[test]
    fn test_assemble_decompose_phase() {
        let repo = make_repo();
        let role_file = repo.path().join(".pipeline").join("backend.md");
        let issue = sample_issue();

        let prompt = assemble(repo.path(), &issue, &role_file, PromptPhase::Decompose, None, 0).unwrap();

        assert!(prompt.text.contains("# Test Repo"));
        assert!(prompt.text.contains("# Backend Standards"));
        assert!(prompt.text.contains("TEST-001"));
        assert!(prompt.text.contains("PHASE: PLAN"));
        assert!(prompt.estimated_tokens > 0);

        // Prompt file written
        let prompt_file = repo.path().join("PIPELINE").join("PROMPT-TEST-001-decompose.md");
        assert!(prompt_file.exists());
    }

    #[test]
    fn test_assemble_with_progress_md() {
        let repo = make_repo();
        std::fs::create_dir_all(repo.path().join("PIPELINE")).unwrap();
        std::fs::write(
            repo.path().join("PIPELINE").join("PROGRESS.md"),
            "# Codebase Progress Notes\n- metering is in metering.rs\n",
        )
        .unwrap();

        let role_file = repo.path().join(".pipeline").join("backend.md");
        let issue = sample_issue();
        let prompt = assemble(repo.path(), &issue, &role_file, PromptPhase::Decompose, None, 0).unwrap();

        assert!(prompt.text.contains("metering is in metering.rs"));
        assert!(prompt.sections.progress_tokens > 0);
    }

    #[test]
    fn test_assemble_missing_claude_md() {
        let dir = TempDir::new().unwrap();
        let role_file = dir.path().join(".pipeline").join("backend.md");
        let issue = sample_issue();
        let err = assemble(dir.path(), &issue, &role_file, PromptPhase::Decompose, None, 0).unwrap_err();
        assert!(err.to_string().contains("No CLAUDE.md"));
    }

    #[test]
    fn test_assemble_execute_phase() {
        let repo = make_repo();
        let role_file = repo.path().join(".pipeline").join("backend.md");
        let issue = sample_issue();
        let story = StoryContext {
            id: "US-001".into(),
            title: "Add hello_world function".into(),
            completed_stories: vec![],
            session_note: "Function goes in lib.rs".into(),
            feedback: None,
        };
        let prompt = assemble(repo.path(), &issue, &role_file, PromptPhase::Execute, Some(&story), 0).unwrap();
        assert!(prompt.text.contains("PHASE: IMPLEMENT US-001"));
        assert!(prompt.text.contains("PIPELINE/PLAN-TEST-001.md"));
    }

    #[test]
    fn test_update_progress() {
        let repo = make_repo();
        update_progress(repo.path(), "TEST-001", "US-001", "hello_world is in lib.rs").unwrap();
        let content = std::fs::read_to_string(repo.path().join("PIPELINE").join("PROGRESS.md")).unwrap();
        assert!(content.contains("hello_world is in lib.rs"));
        assert!(content.contains("TEST-001"));
    }

    #[test]
    fn test_prompt_order() {
        let repo = make_repo();
        let role_file = repo.path().join(".pipeline").join("backend.md");
        let issue = sample_issue();
        let prompt = assemble(repo.path(), &issue, &role_file, PromptPhase::Decompose, None, 0).unwrap();

        // CLAUDE.md must come before role file, role before issue, issue before phase
        let claude_pos = prompt.text.find("# Test Repo").unwrap();
        let role_pos = prompt.text.find("# Backend Standards").unwrap();
        let issue_pos = prompt.text.find("BACKLOG ISSUE").unwrap();
        let phase_pos = prompt.text.find("PHASE: PLAN").unwrap();

        assert!(claude_pos < role_pos, "CLAUDE.md must precede role file");
        assert!(role_pos < issue_pos, "role file must precede issue");
        assert!(issue_pos < phase_pos, "issue must precede phase block");
    }
}
