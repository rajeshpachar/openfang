// US-001: Backlog REST API client.
#![allow(dead_code)]
///
/// Fetches open issues, updates status, and posts comments.
/// HTTP calls are single-attempt; callers that use `let _ = backlog.add_comment(...)` tolerate failures gracefully.
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Backlog status IDs (confirmed via POC-3)
// ---------------------------------------------------------------------------

pub const STATUS_OPEN: u32 = 1;
pub const STATUS_IN_PROGRESS: u32 = 2;
pub const STATUS_RESOLVED: u32 = 3;
pub const STATUS_CLOSED: u32 = 4;

/// Status IDs that the pipeline treats as "already being handled" and skips.
pub const SKIP_STATUS_IDS: &[u32] = &[2, 3, 4, 30576, 30577, 30612];

// ---------------------------------------------------------------------------
// API response types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacklogIssue {
    pub id: u64,
    #[serde(rename = "issueKey")]
    pub key: String,
    pub summary: String,
    pub description: Option<String>,
    pub status: BacklogStatus,
    pub priority: BacklogPriority,
    #[serde(rename = "issueType")]
    pub issue_type: BacklogIssueType,
    #[serde(default)]
    pub category: Vec<BacklogCategory>,
}

impl BacklogIssue {
    pub fn description_text(&self) -> &str {
        self.description.as_deref().unwrap_or("")
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacklogStatus {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacklogPriority {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacklogIssueType {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BacklogCategory {
    pub id: u32,
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct BacklogProject {
    pub id: u64,
    #[serde(rename = "projectKey")]
    pub project_key: String,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct BacklogClient {
    base_url: String,
    api_key: String,
    client: Client,
}

impl BacklogClient {
    pub fn new(base_url: &str, api_key: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.to_string(),
            client: Client::new(),
        }
    }

    /// Resolve a project key (e.g. "MYPROJECT") to its numeric project ID.
    pub async fn resolve_project_id(&self, project_key: &str) -> Result<u64> {
        let url = format!("{}/api/v2/projects?apiKey={}", self.base_url, self.api_key);
        let projects: Vec<BacklogProject> = self
            .client
            .get(&url)
            .send()
            .await
            .context("Failed to fetch Backlog projects")?
            .json()
            .await
            .context("Failed to parse Backlog projects")?;

        projects
            .into_iter()
            .find(|p| p.project_key.eq_ignore_ascii_case(project_key))
            .map(|p| p.id)
            .ok_or_else(|| anyhow::anyhow!("Project '{}' not found in Backlog", project_key))
    }

    /// Fetch the highest-priority open issues for the project (up to 10).
    pub async fn fetch_open_issues(&self, project_id: u64) -> Result<Vec<BacklogIssue>> {
        let url = format!(
            "{}/api/v2/issues?apiKey={}&projectId[]={}&statusId[]=1&sort=priority&order=asc&count=10",
            self.base_url, self.api_key, project_id
        );
        self.get_json(&url).await
    }

    /// Fetch a single issue by key or ID string.
    pub async fn fetch_issue(&self, issue_key: &str) -> Result<BacklogIssue> {
        let url = format!(
            "{}/api/v2/issues/{}?apiKey={}",
            self.base_url, issue_key, self.api_key
        );
        self.get_json(&url).await
    }

    /// Update issue status. `status_id` is a Backlog numeric status ID.
    pub async fn update_status(&self, issue_key: &str, status_id: u32) -> Result<()> {
        let url = format!(
            "{}/api/v2/issues/{}?apiKey={}",
            self.base_url, issue_key, self.api_key
        );
        let resp = self
            .client
            .patch(&url)
            .form(&[("statusId", status_id.to_string())])
            .send()
            .await
            .context("Failed to update issue status")?;

        if !resp.status().is_success() {
            bail!("Status update failed ({}): {}", resp.status(), issue_key);
        }
        Ok(())
    }

    /// Post a comment on an issue.
    pub async fn add_comment(&self, issue_key: &str, content: &str) -> Result<()> {
        let url = format!(
            "{}/api/v2/issues/{}/comments?apiKey={}",
            self.base_url, issue_key, self.api_key
        );
        let resp = self
            .client
            .post(&url)
            .form(&[("content", content)])
            .send()
            .await
            .context("Failed to add Backlog comment")?;

        if !resp.status().is_success() {
            bail!("Add comment failed ({}): {}", resp.status(), issue_key);
        }
        Ok(())
    }

    /// Convenience: get + deserialize JSON with error context.
    async fn get_json<T: for<'de> serde::Deserialize<'de>>(&self, url: &str) -> Result<T> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("Backlog HTTP request failed")?;

        if !resp.status().is_success() {
            bail!("Backlog API returned {}", resp.status());
        }

        resp.json::<T>().await.context("Failed to parse Backlog response")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue(issue_type: &str, categories: &[&str], summary: &str) -> BacklogIssue {
        BacklogIssue {
            id: 1,
            key: "TEST-001".into(),
            summary: summary.into(),
            description: Some("desc".into()),
            status: BacklogStatus { id: 1, name: "Open".into() },
            priority: BacklogPriority { id: 2, name: "High".into() },
            issue_type: BacklogIssueType { id: 1, name: issue_type.into() },
            category: categories
                .iter()
                .enumerate()
                .map(|(i, c)| BacklogCategory { id: i as u32, name: c.to_string() })
                .collect(),
        }
    }

    #[test]
    fn test_description_text_fallback() {
        let mut issue = make_issue("Task", &[], "test");
        issue.description = None;
        assert_eq!(issue.description_text(), "");
    }

    #[test]
    fn test_description_text_present() {
        let issue = make_issue("Task", &[], "test");
        assert_eq!(issue.description_text(), "desc");
    }

    #[test]
    fn test_skip_status_ids_include_in_progress() {
        assert!(SKIP_STATUS_IDS.contains(&STATUS_IN_PROGRESS));
        assert!(SKIP_STATUS_IDS.contains(&STATUS_RESOLVED));
        assert!(SKIP_STATUS_IDS.contains(&STATUS_CLOSED));
    }
}
