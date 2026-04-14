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
    /// Guard violations from the most recent Guard phase run.
    #[serde(default)]
    pub guard_errors: u32,
    #[serde(default)]
    pub guard_warns: u32,
    /// Whether tests passed in the most recent Execute/GapFix run.
    #[serde(default)]
    pub test_passed: bool,
    /// Human rejection notes stored from Gate2 for use in GapFix.
    #[serde(default)]
    pub rejection_notes: Option<String>,
    /// Reason this story was blocked (if status == Blocked).
    #[serde(default)]
    pub block_reason: Option<String>,
    /// Number of soft flags (US-013) received for this story.
    #[serde(default)]
    pub flag_count: u32,
    /// Number of hard rejections (US-014) received for this story.
    #[serde(default)]
    pub rejection_count: u32,
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
    /// Human feedback from Gate1 rejection — passed to next Decompose call.
    #[serde(default)]
    pub pending_feedback: Option<String>,
    /// OpenFang agent_id cached at first gate to avoid re-fetching.
    #[serde(default)]
    pub cached_agent_id: Option<String>,
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
            pending_feedback: None,
            cached_agent_id: None,
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

    /// Add stories from decomposition output.
    pub fn set_stories(&mut self, stories: Vec<StoryState>) {
        self.stories = stories;
        self.current_story_idx = 0;
    }

    /// Mark the current story done and advance to the next.
    /// Returns true if there are more stories remaining.
    pub fn advance_story(&mut self) -> bool {
        if let Some(s) = self.stories.get_mut(self.current_story_idx) {
            s.status = StoryStatus::Done;
        }
        self.current_story_idx += 1;
        self.current_story_idx < self.stories.len()
    }

    /// Mark the current story as blocked.
    pub fn block_story(&mut self, reason: &str) {
        if let Some(s) = self.stories.get_mut(self.current_story_idx) {
            s.status = StoryStatus::Blocked;
            s.block_reason = Some(reason.to_string());
        }
    }

    /// Accumulate cost from a story execution.
    pub fn add_cost(&mut self, usd: f64) {
        self.total_cost_usd += usd;
        if let Some(s) = self.stories.get_mut(self.current_story_idx) {
            s.cost_usd += usd;
        }
    }

    /// Record the commit hash for the current story.
    pub fn set_commit(&mut self, hash: &str) {
        if let Some(s) = self.stories.get_mut(self.current_story_idx) {
            s.commit_hash = Some(hash.to_string());
        }
    }

    /// Increment the cycle counter for the current story.
    pub fn increment_cycle(&mut self) {
        if let Some(s) = self.stories.get_mut(self.current_story_idx) {
            s.cycle_count += 1;
        }
    }

    /// Completed story IDs (for the Execute phase prompt).
    pub fn completed_story_ids(&self) -> Vec<String> {
        self.stories[..self.current_story_idx]
            .iter()
            .filter(|s| s.status == StoryStatus::Done)
            .map(|s| s.id.clone())
            .collect()
    }

    /// Find all state files in `repo_root/PIPELINE/` that are stale.
    pub fn find_stale(repo_root: &Path) -> Vec<Self> {
        let pipeline_dir = repo_root.join("PIPELINE");
        if !pipeline_dir.exists() {
            return vec![];
        }
        std::fs::read_dir(&pipeline_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                let n = e.file_name();
                let s = n.to_string_lossy();
                s.starts_with("STATE-") && s.ends_with(".json")
            })
            .filter_map(|e| {
                let raw = std::fs::read_to_string(e.path()).ok()?;
                serde_json::from_str::<Self>(&raw).ok()
            })
            .filter(|s| s.is_stale())
            .collect()
    }
}

fn state_path(repo_root: &Path, issue_key: &str) -> PathBuf {
    repo_root
        .join("PIPELINE")
        .join(format!("STATE-{}.json", issue_key))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_state() -> PipelineState {
        PipelineState::new(
            "TEST-001",
            "Test issue summary",
            Role::Backend,
            "pipeline/TEST-001",
            PathBuf::from("/tmp/worktree"),
        )
    }

    fn make_story(id: &str) -> StoryState {
        StoryState {
            id: id.to_string(),
            title: format!("Story {}", id),
            status: StoryStatus::Pending,
            session_id: None,
            commit_hash: None,
            files_changed: vec![],
            cost_usd: 0.0,
            cycle_count: 0,
            guard_errors: 0,
            guard_warns: 0,
            test_passed: false,
            rejection_notes: None,
            block_reason: None,
            flag_count: 0,
            rejection_count: 0,
        }
    }

    fn with_stories(n: usize) -> PipelineState {
        let mut s = make_state();
        let stories: Vec<StoryState> = (1..=n)
            .map(|i| make_story(&format!("US-{:03}", i)))
            .collect();
        s.set_stories(stories);
        s
    }

    // -----------------------------------------------------------------------
    // new() / initial state
    // -----------------------------------------------------------------------

    #[test]
    fn test_new_starts_at_decompose() {
        let s = make_state();
        assert_eq!(s.phase, Phase::Decompose);
    }

    #[test]
    fn test_new_has_empty_stories_and_zero_cost() {
        let s = make_state();
        assert!(s.stories.is_empty());
        assert_eq!(s.current_story_idx, 0);
        assert_eq!(s.total_cost_usd, 0.0);
        assert!(s.session_id.is_none());
        assert!(s.pending_feedback.is_none());
    }

    // -----------------------------------------------------------------------
    // set_stories
    // -----------------------------------------------------------------------

    #[test]
    fn test_set_stories_resets_idx_to_zero() {
        let mut s = make_state();
        s.current_story_idx = 3; // simulate mid-run
        s.set_stories(vec![make_story("US-001"), make_story("US-002")]);
        assert_eq!(s.current_story_idx, 0);
        assert_eq!(s.stories.len(), 2);
    }

    // -----------------------------------------------------------------------
    // advance_story
    // -----------------------------------------------------------------------

    #[test]
    fn test_advance_story_marks_current_done() {
        let mut s = with_stories(3);
        s.advance_story();
        assert_eq!(s.stories[0].status, StoryStatus::Done);
    }

    #[test]
    fn test_advance_story_returns_true_when_more_remain() {
        let mut s = with_stories(3);
        assert!(s.advance_story()); // 0→1, stories[1] exists
        assert!(s.advance_story()); // 1→2, stories[2] exists
    }

    #[test]
    fn test_advance_story_returns_false_at_last() {
        let mut s = with_stories(2);
        s.advance_story(); // 0→1
        let more = s.advance_story(); // 1→2, 2 >= 2 → false
        assert!(!more);
    }

    #[test]
    fn test_advance_story_increments_idx() {
        let mut s = with_stories(3);
        s.advance_story();
        assert_eq!(s.current_story_idx, 1);
    }

    // -----------------------------------------------------------------------
    // block_story
    // -----------------------------------------------------------------------

    #[test]
    fn test_block_story_sets_blocked_status() {
        let mut s = with_stories(2);
        s.block_story("cannot find module");
        assert_eq!(s.stories[0].status, StoryStatus::Blocked);
    }

    #[test]
    fn test_block_story_stores_reason() {
        let mut s = with_stories(2);
        s.block_story("compile error in lib.rs");
        assert_eq!(s.stories[0].block_reason.as_deref(), Some("compile error in lib.rs"));
    }

    #[test]
    fn test_block_story_does_not_corrupt_session_id() {
        // Regression: prior bug stored "BLOCKED: reason" in session_id
        let mut s = with_stories(2);
        s.session_id = Some("real-session-abc123".to_string());
        s.block_story("some blocker");
        assert_eq!(s.session_id.as_deref(), Some("real-session-abc123"),
            "block_story must not overwrite session_id");
    }

    // -----------------------------------------------------------------------
    // add_cost
    // -----------------------------------------------------------------------

    #[test]
    fn test_add_cost_accumulates_on_pipeline_state() {
        let mut s = with_stories(2);
        s.add_cost(0.50);
        s.add_cost(0.25);
        assert!((s.total_cost_usd - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_add_cost_accumulates_on_current_story() {
        let mut s = with_stories(2);
        s.add_cost(0.30);
        s.add_cost(0.20);
        assert!((s.stories[0].cost_usd - 0.50).abs() < 1e-9);
    }

    #[test]
    fn test_add_cost_no_panic_with_no_stories() {
        let mut s = make_state(); // no stories
        s.add_cost(1.00); // must not panic
        assert!((s.total_cost_usd - 1.00).abs() < 1e-9);
    }

    // -----------------------------------------------------------------------
    // set_commit / increment_cycle
    // -----------------------------------------------------------------------

    #[test]
    fn test_set_commit_stores_on_current_story() {
        let mut s = with_stories(2);
        s.set_commit("abc1234");
        assert_eq!(s.stories[0].commit_hash.as_deref(), Some("abc1234"));
        assert!(s.stories[1].commit_hash.is_none());
    }

    #[test]
    fn test_increment_cycle_increments_current_story() {
        let mut s = with_stories(2);
        s.increment_cycle();
        s.increment_cycle();
        assert_eq!(s.stories[0].cycle_count, 2);
        assert_eq!(s.stories[1].cycle_count, 0); // untouched
    }

    // -----------------------------------------------------------------------
    // completed_story_ids
    // -----------------------------------------------------------------------

    #[test]
    fn test_completed_story_ids_only_returns_done() {
        let mut s = with_stories(4);
        s.stories[0].status = StoryStatus::Done;
        s.stories[1].status = StoryStatus::InProgress;
        s.stories[2].status = StoryStatus::Done;
        // current_story_idx = 0, so slice [..0] is empty — advance to expose stories
        s.current_story_idx = 3;
        let ids = s.completed_story_ids();
        assert_eq!(ids, vec!["US-001", "US-003"]);
    }

    #[test]
    fn test_completed_story_ids_empty_before_any_advance() {
        let s = with_stories(3);
        // current_story_idx = 0, slice [..0] is empty
        assert!(s.completed_story_ids().is_empty());
    }

    // -----------------------------------------------------------------------
    // is_stale
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_stale_false_for_fresh_state() {
        let s = make_state();
        assert!(!s.is_stale());
    }

    #[test]
    fn test_is_stale_true_for_old_in_progress_state() {
        let mut s = make_state();
        s.phase = Phase::Execute;
        s.last_updated = chrono::Utc::now() - chrono::Duration::hours(25);
        assert!(s.is_stale());
    }

    #[test]
    fn test_is_stale_false_for_complete_even_if_old() {
        let mut s = make_state();
        s.phase = Phase::Complete;
        s.last_updated = chrono::Utc::now() - chrono::Duration::hours(48);
        assert!(!s.is_stale(), "Complete phase is never stale");
    }

    #[test]
    fn test_is_stale_false_for_abandoned_even_if_old() {
        let mut s = make_state();
        s.phase = Phase::Abandoned;
        s.last_updated = chrono::Utc::now() - chrono::Duration::hours(48);
        assert!(!s.is_stale(), "Abandoned phase is never stale");
    }

    // -----------------------------------------------------------------------
    // save / load / exists
    // -----------------------------------------------------------------------

    #[test]
    fn test_exists_false_before_save() {
        let dir = TempDir::new().unwrap();
        assert!(!PipelineState::exists(dir.path(), "TEST-001"));
    }

    #[test]
    fn test_exists_true_after_save() {
        let dir = TempDir::new().unwrap();
        let mut s = make_state();
        s.save(dir.path()).unwrap();
        assert!(PipelineState::exists(dir.path(), "TEST-001"));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut s = with_stories(2);
        s.session_id = Some("sess-xyz".to_string());
        s.total_cost_usd = 1.23;
        s.phase = Phase::Execute;
        s.pending_feedback = Some("needs more detail".to_string());
        s.stories[0].guard_errors = 2;
        s.stories[0].guard_warns = 1;
        s.stories[0].rejection_notes = Some("Too complex".to_string());
        s.save(dir.path()).unwrap();

        let loaded = PipelineState::load(dir.path(), "TEST-001").unwrap();
        assert_eq!(loaded.session_id.as_deref(), Some("sess-xyz"));
        assert!((loaded.total_cost_usd - 1.23).abs() < 1e-9);
        assert_eq!(loaded.phase, Phase::Execute);
        assert_eq!(loaded.pending_feedback.as_deref(), Some("needs more detail"));
        assert_eq!(loaded.stories[0].guard_errors, 2);
        assert_eq!(loaded.stories[0].guard_warns, 1);
        assert_eq!(loaded.stories[0].rejection_notes.as_deref(), Some("Too complex"));
    }

    #[test]
    fn test_load_fails_for_missing_file() {
        let dir = TempDir::new().unwrap();
        assert!(PipelineState::load(dir.path(), "GHOST-999").is_err());
    }

    // -----------------------------------------------------------------------
    // find_stale
    // -----------------------------------------------------------------------

    #[test]
    fn test_find_stale_empty_when_no_pipeline_dir() {
        let dir = TempDir::new().unwrap();
        let stale = PipelineState::find_stale(dir.path());
        assert!(stale.is_empty());
    }

    #[test]
    fn test_find_stale_skips_fresh_states() {
        let dir = TempDir::new().unwrap();
        let mut s = make_state();
        s.phase = Phase::Execute;
        s.save(dir.path()).unwrap();
        let stale = PipelineState::find_stale(dir.path());
        assert!(stale.is_empty(), "fresh state should not be stale");
    }

    #[test]
    fn test_find_stale_returns_old_in_progress_state() {
        let dir = TempDir::new().unwrap();
        let mut s = make_state();
        s.phase = Phase::Execute;
        s.last_updated = chrono::Utc::now() - chrono::Duration::hours(25);
        // Write JSON directly — save() resets last_updated to now, defeating the test.
        let pipeline_dir = dir.path().join("PIPELINE");
        std::fs::create_dir_all(&pipeline_dir).unwrap();
        let json = serde_json::to_string_pretty(&s).unwrap();
        std::fs::write(pipeline_dir.join("STATE-TEST-001.json"), json).unwrap();

        let stale = PipelineState::find_stale(dir.path());
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].issue_key, "TEST-001");
    }

    #[test]
    fn test_find_stale_excludes_complete() {
        let dir = TempDir::new().unwrap();
        let mut s = make_state();
        s.phase = Phase::Complete;
        s.last_updated = chrono::Utc::now() - chrono::Duration::hours(48);
        s.save(dir.path()).unwrap();
        let stale = PipelineState::find_stale(dir.path());
        assert!(stale.is_empty());
    }
}
