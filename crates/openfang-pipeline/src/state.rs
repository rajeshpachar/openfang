// Phase 1 skeleton: PipelineState methods used in Phase 2.
#![allow(dead_code)]

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Decompose,
    Gate1,
    Execute,
    Guard,
    Gate2,
    GapFix,
    Pr,
    Complete,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Backend,
    Frontend,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoryState {
    pub id: String,
    pub title: String,
    pub status: StoryStatus,
    pub session_id: Option<String>,
    pub commit_hash: Option<String>,
    pub files_changed: Vec<String>,
    pub cost_usd: f64,
    pub cycle_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StoryStatus {
    Pending,
    InProgress,
    Done,
    Flagged,
    Rejected,
    Blocked,
}

/// Per-issue pipeline state — written to `PIPELINE/STATE-{issueKey}.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineState {
    /// Issue key (e.g. "OFANG-123").
    pub issue_key: String,
    /// Issue summary (title).
    pub issue_summary: String,
    /// Current pipeline phase.
    pub phase: Phase,
    /// Role classification.
    pub role: Role,
    /// Branch name (e.g. "pipeline/OFANG-123").
    pub branch: String,
    /// Git worktree path (e.g. "~/.pipeline/worktrees/OFANG-123/").
    pub worktree_path: PathBuf,
    /// Claude session ID for the current active session.
    pub session_id: Option<String>,
    /// Stories derived from decomposition.
    pub stories: Vec<StoryState>,
    /// Index into `stories` of the currently executing story.
    pub current_story_idx: usize,
    /// Accumulated cost for this issue in USD.
    pub total_cost_usd: f64,
    /// Draft PR URL (set after Gate 1 approve).
    pub pr_url: Option<String>,
    /// OpenFang approval gate ID currently pending (if any).
    pub pending_gate_id: Option<String>,
    /// Timestamp of last state update.
    pub last_updated: DateTime<Utc>,
    /// Timestamp when the pipeline first picked up this issue.
    pub started_at: DateTime<Utc>,
}

impl PipelineState {
    pub fn new(
        issue_key: impl Into<String>,
        issue_summary: impl Into<String>,
        role: Role,
        branch: impl Into<String>,
        worktree_path: PathBuf,
    ) -> Self {
        let now = Utc::now();
        Self {
            issue_key: issue_key.into(),
            issue_summary: issue_summary.into(),
            phase: Phase::Decompose,
            role,
            branch: branch.into(),
            worktree_path,
            session_id: None,
            stories: Vec::new(),
            current_story_idx: 0,
            total_cost_usd: 0.0,
            pr_url: None,
            pending_gate_id: None,
            last_updated: now,
            started_at: now,
        }
    }

    /// Load state from `PIPELINE/STATE-{issueKey}.json` in `repo_root`.
    pub fn load(repo_root: &Path, issue_key: &str) -> Result<Self> {
        let path = state_path(repo_root, issue_key);
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read state file {}", path.display()))?;
        serde_json::from_str(&raw)
            .with_context(|| format!("Failed to parse state file {}", path.display()))
    }

    /// Save state to `PIPELINE/STATE-{issueKey}.json` in `repo_root`.
    pub fn save(&mut self, repo_root: &Path) -> Result<()> {
        self.last_updated = Utc::now();
        let path = state_path(repo_root, &self.issue_key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)
            .context("Failed to serialise pipeline state")?;
        std::fs::write(&path, json)
            .with_context(|| format!("Failed to write state file {}", path.display()))
    }

    /// Return true if state file exists for this issue.
    pub fn exists(repo_root: &Path, issue_key: &str) -> bool {
        state_path(repo_root, issue_key).exists()
    }

    /// Current story (if any).
    pub fn current_story(&self) -> Option<&StoryState> {
        self.stories.get(self.current_story_idx)
    }

    /// Mark state as stale — used for crash recovery detection.
    pub fn is_stale(&self) -> bool {
        let age = Utc::now() - self.last_updated;
        age.num_hours() > 24 && self.phase != Phase::Complete && self.phase != Phase::Abandoned
    }
}

fn state_path(repo_root: &Path, issue_key: &str) -> PathBuf {
    repo_root
        .join("PIPELINE")
        .join(format!("STATE-{}.json", issue_key))
}
