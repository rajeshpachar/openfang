// US-015/016: Session continuity — handoff notes, expired session recovery, progress truncation.
#![allow(dead_code)]
///
/// After every approved story, the handoff_note from Claude's JSON output is saved to:
///   PIPELINE/HANDOFF-{key}-{storyId}.md
///
/// When a fresh session is needed (Ralph loop, expired session, budget exhaustion):
///   1. Load all handoff files for completed stories, in story order.
///   2. Build a preamble block from them.
///   3. Prepend to the next Execute prompt so Claude re-orients.
///
/// Expired session detection (POC-1):
///   exit code 1 + stderr contains "No conversation found with session ID: {id}"
///
/// Progress truncation:
///   PIPELINE/PROGRESS.md is capped at MAX_PROGRESS_TOKENS. When exceeded,
///   the oldest entries are dropped, keeping the most recent content.
use anyhow::{Context, Result};
use std::path::Path;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Maximum PROGRESS.md size in tokens (4 chars ≈ 1 token).
pub const MAX_PROGRESS_TOKENS: usize = 2_000;
/// Token threshold above which a fresh session is triggered proactively.
pub const TOKEN_FRESH_SESSION_THRESHOLD: usize = 70_000;
/// Default max stories per session if config value is out of range.
pub const DEFAULT_MAX_STORIES_PER_SESSION: u32 = 5;

// ---------------------------------------------------------------------------
// Expired session detection (US-015)
// ---------------------------------------------------------------------------

/// Returns true if Claude's stderr output indicates an expired / invalid session ID.
/// Signal (POC-1): "No conversation found with session ID: {id}"
pub fn is_expired_session_error(stderr: &str) -> bool {
    stderr.contains("No conversation found with session ID")
}

// ---------------------------------------------------------------------------
// Config validation (US-016)
// ---------------------------------------------------------------------------

/// Validate `max_stories_per_session` is in [1, 20].
/// Returns the value if valid, otherwise emits a warning and returns DEFAULT (5).
pub fn validate_max_stories_per_session(value: u32) -> u32 {
    if (1..=20).contains(&value) {
        value
    } else {
        eprintln!(
            "  WARN max_stories_per_session={} is out of range [1,20] — using default {}",
            value, DEFAULT_MAX_STORIES_PER_SESSION
        );
        DEFAULT_MAX_STORIES_PER_SESSION
    }
}

// ---------------------------------------------------------------------------
// Handoff notes (US-016)
// ---------------------------------------------------------------------------

/// Save a story's `handoff_note` to `PIPELINE/HANDOFF-{key}-{story_id}.md`.
pub fn save_handoff_note(
    repo_root: &Path,
    issue_key: &str,
    story_id: &str,
    note: &str,
) -> Result<()> {
    let dir = pipeline_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("HANDOFF-{}-{}.md", issue_key, story_id));
    let content = format!(
        "# Handoff — {} {}\n\n{}\n",
        issue_key, story_id, note.trim()
    );
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write handoff note to {}", path.display()))
}

/// Load handoff notes for the given story IDs, in order.
/// Missing files are skipped silently (story may have been a fresh-session start).
pub fn load_handoff_notes(
    repo_root: &Path,
    issue_key: &str,
    story_ids: &[String],
) -> Vec<(String, String)> {
    story_ids
        .iter()
        .filter_map(|id| {
            let path = pipeline_dir(repo_root)
                .join(format!("HANDOFF-{}-{}.md", issue_key, id));
            let text = std::fs::read_to_string(path).ok()?;
            Some((id.clone(), text))
        })
        .collect()
}

/// Build a preamble block from loaded handoff notes for injection into fresh-session prompts.
/// Returns an empty string if there are no notes.
pub fn build_handoff_preamble(notes: &[(String, String)]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out =
        String::from("## Context From Completed Stories (session continuity)\n\n");
    for (id, text) in notes {
        out.push_str(&format!("### {}\n{}\n\n", id, text.trim()));
    }
    out
}

// ---------------------------------------------------------------------------
// PROGRESS.md truncation (US-016)
// ---------------------------------------------------------------------------

/// Truncate `PIPELINE/PROGRESS.md` to the most recent entries fitting within
/// `MAX_PROGRESS_TOKENS`. Returns true if truncation occurred.
///
/// Strategy: if content exceeds the token budget, drop from the front until it fits,
/// aligning to the next `## ` story section boundary to avoid split entries.
pub fn truncate_progress_if_needed(repo_root: &Path) -> Result<bool> {
    let path = pipeline_dir(repo_root).join("PROGRESS.md");
    if !path.exists() {
        return Ok(false);
    }

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let max_chars = MAX_PROGRESS_TOKENS * 4;
    if content.len() <= max_chars {
        return Ok(false);
    }

    // Find the last `max_chars` of the content, then align forward to the next `## ` boundary.
    let tail_start = content.len() - max_chars;
    let aligned_start = content[tail_start..]
        .find("\n## ")
        .map(|off| tail_start + off + 1) // +1 to include the newline
        .unwrap_or(tail_start);

    let kept = &content[aligned_start..];
    let truncated = format!(
        "<!-- Truncated: oldest entries removed to stay within token budget -->\n\n{}\n",
        kept.trim_start()
    );

    std::fs::write(&path, &truncated)
        .with_context(|| "Failed to write truncated PROGRESS.md".to_string())?;

    Ok(true)
}

fn pipeline_dir(repo_root: &Path) -> std::path::PathBuf {
    repo_root.join("PIPELINE")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // -----------------------------------------------------------------------
    // is_expired_session_error
    // -----------------------------------------------------------------------

    #[test]
    fn test_expired_session_detected() {
        let stderr = "Error: No conversation found with session ID: abc-123-xyz";
        assert!(is_expired_session_error(stderr));
    }

    #[test]
    fn test_expired_session_not_detected_for_other_errors() {
        assert!(!is_expired_session_error("Error: API rate limit exceeded"));
        assert!(!is_expired_session_error("Budget exhausted"));
        assert!(!is_expired_session_error(""));
    }

    // -----------------------------------------------------------------------
    // validate_max_stories_per_session
    // -----------------------------------------------------------------------

    #[test]
    fn test_valid_values_pass_through() {
        assert_eq!(validate_max_stories_per_session(1), 1);
        assert_eq!(validate_max_stories_per_session(5), 5);
        assert_eq!(validate_max_stories_per_session(20), 20);
    }

    #[test]
    fn test_zero_returns_default() {
        assert_eq!(
            validate_max_stories_per_session(0),
            DEFAULT_MAX_STORIES_PER_SESSION
        );
    }

    #[test]
    fn test_above_20_returns_default() {
        assert_eq!(
            validate_max_stories_per_session(21),
            DEFAULT_MAX_STORIES_PER_SESSION
        );
        assert_eq!(
            validate_max_stories_per_session(100),
            DEFAULT_MAX_STORIES_PER_SESSION
        );
    }

    // -----------------------------------------------------------------------
    // save / load handoff notes
    // -----------------------------------------------------------------------

    #[test]
    fn test_save_and_load_handoff_note() {
        let dir = TempDir::new().unwrap();
        save_handoff_note(
            dir.path(),
            "OFANG-001",
            "US-002",
            "Completed route handler. Changed routes.rs, handlers.rs. Next: add tests.",
        )
        .unwrap();

        let notes = load_handoff_notes(
            dir.path(),
            "OFANG-001",
            &["US-002".to_string()],
        );
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].0, "US-002");
        assert!(notes[0].1.contains("route handler"));
    }

    #[test]
    fn test_load_handoff_notes_skips_missing_files() {
        let dir = TempDir::new().unwrap();
        save_handoff_note(dir.path(), "OFANG-001", "US-001", "Completed US-001.").unwrap();

        let ids = ["US-001", "US-002", "US-003"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let notes = load_handoff_notes(dir.path(), "OFANG-001", &ids);
        assert_eq!(notes.len(), 1, "Only US-001 has a file; US-002 and US-003 skipped");
    }

    #[test]
    fn test_load_handoff_notes_preserves_order() {
        let dir = TempDir::new().unwrap();
        save_handoff_note(dir.path(), "OFANG-001", "US-001", "Story 1").unwrap();
        save_handoff_note(dir.path(), "OFANG-001", "US-002", "Story 2").unwrap();
        save_handoff_note(dir.path(), "OFANG-001", "US-003", "Story 3").unwrap();

        let ids = ["US-001", "US-002", "US-003"]
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>();
        let notes = load_handoff_notes(dir.path(), "OFANG-001", &ids);
        assert_eq!(notes[0].0, "US-001");
        assert_eq!(notes[1].0, "US-002");
        assert_eq!(notes[2].0, "US-003");
    }

    // -----------------------------------------------------------------------
    // build_handoff_preamble
    // -----------------------------------------------------------------------

    #[test]
    fn test_preamble_empty_when_no_notes() {
        let preamble = build_handoff_preamble(&[]);
        assert!(preamble.is_empty());
    }

    #[test]
    fn test_preamble_contains_story_ids_and_text() {
        let notes = vec![
            ("US-001".to_string(), "# Handoff\nCompleted auth route.".to_string()),
            ("US-002".to_string(), "# Handoff\nAdded tests.".to_string()),
        ];
        let preamble = build_handoff_preamble(&notes);
        assert!(preamble.contains("US-001"));
        assert!(preamble.contains("US-002"));
        assert!(preamble.contains("Completed auth route"));
    }

    // -----------------------------------------------------------------------
    // truncate_progress_if_needed
    // -----------------------------------------------------------------------

    #[test]
    fn test_no_truncation_for_small_file() {
        let dir = TempDir::new().unwrap();
        let pipeline_dir = dir.path().join("PIPELINE");
        std::fs::create_dir_all(&pipeline_dir).unwrap();
        std::fs::write(pipeline_dir.join("PROGRESS.md"), "## US-001\nSmall content.\n").unwrap();

        let truncated = truncate_progress_if_needed(dir.path()).unwrap();
        assert!(!truncated, "Small file should not be truncated");
    }

    #[test]
    fn test_truncation_for_oversized_file() {
        let dir = TempDir::new().unwrap();
        let pipeline_dir = dir.path().join("PIPELINE");
        std::fs::create_dir_all(&pipeline_dir).unwrap();

        // Build a file > MAX_PROGRESS_TOKENS * 4 chars
        let big_content: String = (1..=50)
            .map(|i| format!("## US-{:03}\n{}\n\n", i, "x".repeat(500)))
            .collect();
        std::fs::write(pipeline_dir.join("PROGRESS.md"), &big_content).unwrap();

        let truncated = truncate_progress_if_needed(dir.path()).unwrap();
        assert!(truncated, "Large file should be truncated");

        let result = std::fs::read_to_string(pipeline_dir.join("PROGRESS.md")).unwrap();
        assert!(result.len() <= MAX_PROGRESS_TOKENS * 4 + 200, "Truncated file should fit token budget");
        assert!(result.contains("Truncated"), "Should have truncation notice");
    }

    #[test]
    fn test_no_truncation_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let result = truncate_progress_if_needed(dir.path()).unwrap();
        assert!(!result);
    }
}
