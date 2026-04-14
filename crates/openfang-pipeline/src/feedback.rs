// US-013/014: Flag and reject feedback — differentiation and file persistence.
#![allow(dead_code)]
///
/// Gate2 rejection notes come in two flavours:
///   "FLAG: {text}"  → soft feedback; Claude amends the commit (US-013)
///   "{text}"        → hard reject; Claude reverts and redoes from scratch (US-014)
///
/// Saved files:
///   PIPELINE/FEEDBACK-{key}-{storyId}.md           ← flag feedback (latest wins)
///   PIPELINE/FEEDBACK-{key}-{storyId}-reject-{n}.md ← rejection reason per cycle
use anyhow::{Context, Result};
use std::path::Path;

// ---------------------------------------------------------------------------
// Flag vs Reject discrimination
// ---------------------------------------------------------------------------

/// Returns true if the gate2 notes represent a soft flag (`FLAG:` prefix).
/// Case-insensitive, leading whitespace ignored.
pub fn is_flag(notes: &str) -> bool {
    notes.trim().to_ascii_uppercase().starts_with("FLAG:")
}

/// Extract human-readable feedback from a FLAG note (strips `FLAG:` prefix).
pub fn extract_flag_text(notes: &str) -> String {
    let t = notes.trim();
    // Try exact prefix match first, then lowercase variant
    t.strip_prefix("FLAG:")
        .or_else(|| t.strip_prefix("flag:"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| t.to_string())
}

// ---------------------------------------------------------------------------
// File persistence
// ---------------------------------------------------------------------------

/// Save flag feedback to `PIPELINE/FEEDBACK-{key}-{story_id}.md`.
/// Overwrites previous feedback for the same story (latest wins).
pub fn save_flag_feedback(
    repo_root: &Path,
    issue_key: &str,
    story_id: &str,
    feedback: &str,
) -> Result<()> {
    let dir = pipeline_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("FEEDBACK-{}-{}.md", issue_key, story_id));
    let content = format!(
        "# Flag Feedback — {} {}\n\n{}\n",
        issue_key, story_id, feedback
    );
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write flag feedback to {}", path.display()))
}

/// Save hard-reject reason to `PIPELINE/FEEDBACK-{key}-{story_id}-reject-{cycle}.md`.
pub fn save_rejection_reason(
    repo_root: &Path,
    issue_key: &str,
    story_id: &str,
    cycle: u32,
    reason: &str,
) -> Result<()> {
    let dir = pipeline_dir(repo_root);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "FEEDBACK-{}-{}-reject-{}.md",
        issue_key, story_id, cycle
    ));
    let content = format!(
        "# Rejection — {} {} (cycle {})\n\n{}\n",
        issue_key, story_id, cycle, reason
    );
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write rejection reason to {}", path.display()))
}

/// Load flag feedback from file (for GapFix prompt injection after crash/resume).
/// Returns None if the file does not exist.
pub fn load_flag_feedback(
    repo_root: &Path,
    issue_key: &str,
    story_id: &str,
) -> Option<String> {
    let path = pipeline_dir(repo_root)
        .join(format!("FEEDBACK-{}-{}.md", issue_key, story_id));
    std::fs::read_to_string(path).ok()
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
    // is_flag
    // -----------------------------------------------------------------------

    #[test]
    fn test_is_flag_uppercase_prefix() {
        assert!(is_flag("FLAG: needs more error handling"));
        assert!(is_flag("FLAG:no space is fine too"));
    }

    #[test]
    fn test_is_flag_lowercase_prefix() {
        // is_flag is case-insensitive via to_ascii_uppercase
        assert!(is_flag("flag: lower case"));
        assert!(is_flag("Flag: mixed case"));
    }

    #[test]
    fn test_is_flag_with_leading_whitespace() {
        assert!(is_flag("  FLAG: trimmed"));
    }

    #[test]
    fn test_is_flag_false_for_plain_rejection() {
        assert!(!is_flag("This is a plain rejection"));
        assert!(!is_flag("The handler is too long"));
        assert!(!is_flag(""));
    }

    #[test]
    fn test_is_flag_false_for_flag_in_body_not_prefix() {
        assert!(!is_flag("Please note the FLAG: is in the middle"));
    }

    // -----------------------------------------------------------------------
    // extract_flag_text
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_flag_text_strips_prefix() {
        assert_eq!(extract_flag_text("FLAG: needs error handling"), "needs error handling");
        assert_eq!(extract_flag_text("flag: lowercase"), "lowercase");
    }

    #[test]
    fn test_extract_flag_text_trims_result() {
        assert_eq!(extract_flag_text("FLAG:   lots of spaces   "), "lots of spaces");
    }

    #[test]
    fn test_extract_flag_text_no_prefix_returns_as_is() {
        assert_eq!(extract_flag_text("plain rejection text"), "plain rejection text");
    }

    // -----------------------------------------------------------------------
    // save / load roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_save_and_load_flag_feedback() {
        let dir = TempDir::new().unwrap();
        save_flag_feedback(dir.path(), "OFANG-001", "US-003", "Add null check").unwrap();
        let loaded = load_flag_feedback(dir.path(), "OFANG-001", "US-003").unwrap();
        assert!(loaded.contains("Add null check"));
    }

    #[test]
    fn test_load_flag_feedback_missing_returns_none() {
        let dir = TempDir::new().unwrap();
        assert!(load_flag_feedback(dir.path(), "OFANG-001", "US-999").is_none());
    }

    #[test]
    fn test_save_flag_feedback_overwrites_previous() {
        let dir = TempDir::new().unwrap();
        save_flag_feedback(dir.path(), "OFANG-001", "US-003", "first feedback").unwrap();
        save_flag_feedback(dir.path(), "OFANG-001", "US-003", "second feedback").unwrap();
        let loaded = load_flag_feedback(dir.path(), "OFANG-001", "US-003").unwrap();
        assert!(loaded.contains("second feedback"));
        assert!(!loaded.contains("first feedback"));
    }

    #[test]
    fn test_save_rejection_reason_creates_file() {
        let dir = TempDir::new().unwrap();
        save_rejection_reason(dir.path(), "OFANG-001", "US-003", 2, "Tests still failing").unwrap();
        let path = dir.path()
            .join("PIPELINE")
            .join("FEEDBACK-OFANG-001-US-003-reject-2.md");
        assert!(path.exists());
        let content = std::fs::read_to_string(path).unwrap();
        assert!(content.contains("Tests still failing"));
        assert!(content.contains("cycle 2"));
    }
}
