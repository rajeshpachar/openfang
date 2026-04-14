/// US-002: Deterministic role classifier.
///
/// Maps Backlog issue metadata to Backend | Frontend using label matching.
/// Matching order: issueType.name → category names → summary keywords (all case-insensitive).
/// Fullstack (both labels match) is rejected in v1 — pipeline skips the issue.
use crate::backlog::BacklogIssue;
use crate::config::RoleLabels;
use crate::state::Role;

#[derive(Debug, PartialEq, Eq)]
pub enum ClassifyResult {
    /// Clearly one role.
    Single(Role),
    /// Both backend and frontend labels matched — not supported in v1.
    Fullstack,
    /// No label matched — pipeline must skip this issue.
    Unclassified,
}

/// Classify the role of a Backlog issue against the configured label lists.
pub fn classify(issue: &BacklogIssue, labels: &RoleLabels) -> ClassifyResult {
    let backend_score = count_matches(issue, &labels.backend);
    let frontend_score = count_matches(issue, &labels.frontend);

    match (backend_score > 0, frontend_score > 0) {
        (true, true) => ClassifyResult::Fullstack,
        (true, false) => ClassifyResult::Single(Role::Backend),
        (false, true) => ClassifyResult::Single(Role::Frontend),
        (false, false) => ClassifyResult::Unclassified,
    }
}

/// Count how many labels from `labels` appear in issue metadata.
fn count_matches(issue: &BacklogIssue, labels: &[String]) -> usize {
    let type_name = issue.issue_type.name.to_lowercase();
    let categories: Vec<String> = issue.category.iter().map(|c| c.name.to_lowercase()).collect();
    let summary = issue.summary.to_lowercase();

    labels.iter().filter(|label| {
        let l = label.to_lowercase();
        type_name.contains(&l)
            || categories.iter().any(|c| c.contains(&l))
            || summary.contains(&l)
    }).count()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backlog::{BacklogCategory, BacklogIssue, BacklogIssueType, BacklogPriority, BacklogStatus};

    fn make_issue(type_name: &str, categories: &[&str], summary: &str) -> BacklogIssue {
        BacklogIssue {
            id: 1,
            key: "TEST-001".into(),
            summary: summary.into(),
            description: Some("desc".into()),
            status: BacklogStatus { id: 1, name: "Open".into() },
            priority: BacklogPriority { id: 2, name: "High".into() },
            issue_type: BacklogIssueType { id: 1, name: type_name.into() },
            category: categories
                .iter()
                .enumerate()
                .map(|(i, c)| BacklogCategory { id: i as u32, name: c.to_string() })
                .collect(),
        }
    }

    fn default_labels() -> RoleLabels {
        RoleLabels::default()
    }

    #[test]
    fn test_classify_backend_via_issue_type() {
        let issue = make_issue("Backend Task", &[], "Add budget tracking");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Backend));
    }

    #[test]
    fn test_classify_frontend_via_issue_type() {
        let issue = make_issue("Frontend Task", &[], "Add budget tracking");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Frontend));
    }

    #[test]
    fn test_classify_backend_via_category() {
        let issue = make_issue("Task", &["api", "feature"], "some task");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Backend));
    }

    #[test]
    fn test_classify_frontend_via_category() {
        let issue = make_issue("Task", &["ui", "components"], "some task");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Frontend));
    }

    #[test]
    fn test_classify_backend_via_summary_keyword() {
        let issue = make_issue("Task", &[], "Fix database connection pool");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Backend));
    }

    #[test]
    fn test_classify_frontend_via_summary_keyword() {
        let issue = make_issue("Task", &[], "Fix dashboard layout overflow");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Frontend));
    }

    #[test]
    fn test_classify_unclassified_when_no_match() {
        let issue = make_issue("Task", &[], "Weekly team meeting notes");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Unclassified);
    }

    #[test]
    fn test_classify_fullstack_when_both_match() {
        let issue = make_issue("Task", &["backend", "ui"], "Full feature");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Fullstack);
    }

    #[test]
    fn test_classify_case_insensitive() {
        let issue = make_issue("BACKEND", &[], "some task");
        assert_eq!(classify(&issue, &default_labels()), ClassifyResult::Single(Role::Backend));
    }

    #[test]
    fn test_classify_custom_labels() {
        let labels = RoleLabels {
            backend: vec!["kernel".into(), "grpc".into()],
            frontend: vec!["react".into()],
        };
        let issue = make_issue("gRPC Service", &[], "Add streaming endpoint");
        assert_eq!(classify(&issue, &labels), ClassifyResult::Single(Role::Backend));
    }
}
