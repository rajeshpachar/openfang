// US-012: OpenFang approval gate client.
#![allow(dead_code)]
///
/// Posts a gate to the OpenFang Approval System and polls until the human decides.
/// Key constraints confirmed by POC-10:
///   - Max timeout_secs: 300 (hardcoded server limit)
///   - No GET /api/approvals/{id} — must use list + filter by id
///   - Reject endpoint: POST /api/approvals/{id}/reject  (not /deny)
///   - Poll every 30 seconds; re-post new approval on expiry
///   - requested_at must be current UTC at time of POST (stale → immediate expiry)
use anyhow::{bail, Context, Result};
use colored::Colorize;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct ApprovalRequest<'a> {
    id: String,
    agent_id: &'a str,
    tool_name: &'static str,
    description: String,
    action_summary: String,
    risk_level: &'static str,
    requested_at: String,
    timeout_secs: u64,
}

#[derive(Debug, Deserialize, Clone)]
struct ApprovalRecord {
    id: String,
    status: String,
    notes: Option<String>,
}

/// Decision returned after a gate resolves.
#[derive(Debug)]
pub enum GateDecision {
    Approved,
    /// Rejected — `notes` may contain "FLAG: {feedback}" for rework.
    Rejected { notes: Option<String> },
}

/// Guard violation summary passed to the gate description.
pub struct GateSummary<'a> {
    pub issue_key: &'a str,
    pub story_id: &'a str,
    pub story_title: &'a str,
    pub guard_pass: bool,
    pub error_count: usize,
    pub warn_count: usize,
    pub test_passed: bool,
    pub commit_hash: &'a str,
    pub files_changed: usize,
    pub cost_this_issue: f64,
    pub branch: &'a str,
    pub cycle: u32,
    pub max_cycles: u32,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct GateClient {
    openfang_url: String,
    agent_id: String,
    client: Client,
}

impl GateClient {
    /// Create a new gate client. Fetches the agent_id from OpenFang at construction.
    pub async fn new(openfang_url: &str) -> Result<Self> {
        let client = Client::new();
        let agent_id = fetch_agent_id(&client, openfang_url).await?;
        Ok(Self {
            openfang_url: openfang_url.trim_end_matches('/').to_string(),
            agent_id,
            client,
        })
    }

    /// Create a client with a known agent_id (useful for testing / resuming).
    pub fn with_agent_id(openfang_url: &str, agent_id: &str) -> Self {
        Self {
            openfang_url: openfang_url.trim_end_matches('/').to_string(),
            agent_id: agent_id.to_string(),
            client: Client::new(),
        }
    }

    /// Post the gate approval request and block until the human decides.
    /// Re-posts automatically on every 5-minute expiry. Waits indefinitely.
    pub async fn post_and_wait(&self, summary: &GateSummary<'_>) -> Result<GateDecision> {
        let description = build_description(summary);
        let action_summary = format!(
            "Continue pipeline after {} for {}",
            summary.story_id, summary.issue_key
        );

        print_gate_block(summary);

        loop {
            let gate_id = self.post_gate(&description, &action_summary).await?;
            println!(
                "  {} Approval posted → {} | waiting...",
                "→".cyan(),
                gate_id.dimmed()
            );

            let decision = self.poll(&gate_id).await?;
            match decision {
                PollResult::Approved => return Ok(GateDecision::Approved),
                PollResult::Rejected { notes } => return Ok(GateDecision::Rejected { notes }),
                PollResult::Expired => {
                    // Re-post with fresh timestamp
                    println!("  {} Gate expired — re-posting", "↻".yellow());
                    continue;
                }
            }
        }
    }

    async fn post_gate(&self, description: &str, action_summary: &str) -> Result<String> {
        let gate_id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

        let req = ApprovalRequest {
            id: gate_id.clone(),
            agent_id: &self.agent_id,
            tool_name: "pipeline_gate",
            description: description.to_string(),
            action_summary: action_summary.to_string(),
            risk_level: "High",
            requested_at: now,
            timeout_secs: 300,
        };

        let url = format!("{}/api/approvals", self.openfang_url);
        let resp = self
            .client
            .post(&url)
            .json(&req)
            .send()
            .await
            .context("Failed to post approval gate to OpenFang")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("POST /api/approvals returned {}: {}", status, body);
        }

        Ok(gate_id)
    }

    /// Poll the approvals list every 30 seconds until the gate resolves or expires.
    async fn poll(&self, gate_id: &str) -> Result<PollResult> {
        let url = format!("{}/api/approvals", self.openfang_url);
        let mut elapsed_secs: u64 = 0;

        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            elapsed_secs += 30;

            let approvals: Vec<ApprovalRecord> = self
                .client
                .get(&url)
                .send()
                .await
                .context("Failed to poll /api/approvals")?
                .json()
                .await
                .context("Failed to parse approvals list")?;

            let record = approvals.iter().find(|a| a.id == gate_id);

            let mins = elapsed_secs / 60;
            let secs = elapsed_secs % 60;
            println!(
                "  {} Waiting... (gate/{} | {} | {}m {}s elapsed)",
                "[GATE]".dimmed(),
                &gate_id[..8],
                record.map(|r| r.status.as_str()).unwrap_or("pending"),
                mins,
                secs
            );

            match record {
                Some(r) if r.status == "approved" => return Ok(PollResult::Approved),
                Some(r) if r.status == "rejected" => {
                    return Ok(PollResult::Rejected { notes: r.notes.clone() })
                }
                Some(r) if r.status == "expired" => return Ok(PollResult::Expired),
                _ => {
                    // pending or not yet visible — continue polling
                }
            }
        }
    }
}

#[derive(Debug)]
enum PollResult {
    Approved,
    Rejected { notes: Option<String> },
    Expired,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn fetch_agent_id(client: &Client, openfang_url: &str) -> Result<String> {
    let url = format!("{}/api/agents", openfang_url.trim_end_matches('/'));
    let agents: Vec<serde_json::Value> = client
        .get(&url)
        .send()
        .await
        .context("Failed to fetch agents from OpenFang")?
        .json()
        .await
        .context("Failed to parse agents response")?;

    agents
        .into_iter()
        .next()
        .and_then(|a| a["id"].as_str().map(|s| s.to_string()))
        .ok_or_else(|| anyhow::anyhow!("No agents found in OpenFang — is the daemon running?"))
}

fn build_description(s: &GateSummary<'_>) -> String {
    let guard_status = if s.guard_pass {
        format!("Guards: {} warns", s.warn_count)
    } else {
        format!("Guards: {} errors, {} warns", s.error_count, s.warn_count)
    };
    let test_status = if s.test_passed { "Tests: passed" } else { "Tests: FAILED" };
    format!(
        "Story {} complete — {} | {} | Files: {} | Cost: ${:.2} | Cycle: {}/{}",
        s.story_id, guard_status, test_status, s.files_changed, s.cost_this_issue, s.cycle, s.max_cycles
    )
}

fn print_gate_block(s: &GateSummary<'_>) {
    let sep = "━".repeat(55);
    println!("\n{}", sep.bold());
    println!(
        " {}  {}  {}: {}",
        "CHECKPOINT".bold(),
        s.issue_key.cyan().bold(),
        s.story_id.bold(),
        s.story_title
    );
    println!(
        " Role: backend · Cycle: {}/{} · Branch: {}",
        s.cycle, s.max_cycles, s.branch.cyan()
    );
    println!("{}", sep.bold());

    let guard_line = if s.guard_pass {
        format!(" {}  {} errors  {} warns", "GUARDS:".bold(), s.error_count, s.warn_count)
    } else {
        format!(" {}  {} errors  {} warns", "GUARDS:".red().bold(), s.error_count, s.warn_count)
    };
    println!("{}", guard_line);

    let test_line = if s.test_passed {
        format!(" {}  passed {}", "TESTS:".bold(), "✓".green())
    } else {
        format!(" {}  {} FAILED", "TESTS:".bold(), "✗".red())
    };
    println!("{}", test_line);

    println!(
        " {}  ${:.2} this issue",
        "COST:".bold(),
        s.cost_this_issue
    );
    println!();
    println!(" Approve: POST http://localhost:50051/api/approvals/<id>/approve");
    println!(" Reject:  POST http://localhost:50051/api/approvals/<id>/reject");
    println!("{}\n", sep.bold());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_summary() -> GateSummary<'static> {
        GateSummary {
            issue_key: "OFANG-101",
            story_id: "US-001",
            story_title: "Add budget tracking",
            guard_pass: true,
            error_count: 0,
            warn_count: 1,
            test_passed: true,
            commit_hash: "abc1234",
            files_changed: 3,
            cost_this_issue: 0.43,
            branch: "pipeline/OFANG-101",
            cycle: 1,
            max_cycles: 3,
        }
    }

    #[test]
    fn test_build_description_pass() {
        let s = sample_summary();
        let desc = build_description(&s);
        assert!(desc.contains("US-001"));
        assert!(desc.contains("Guards: 1 warns"));
        assert!(desc.contains("Tests: passed"));
        assert!(desc.contains("$0.43"));
    }

    #[test]
    fn test_build_description_fail() {
        let mut s = sample_summary();
        s.guard_pass = false;
        s.error_count = 2;
        s.test_passed = false;
        let desc = build_description(&s);
        assert!(desc.contains("2 errors"));
        assert!(desc.contains("Tests: FAILED"));
    }
}
