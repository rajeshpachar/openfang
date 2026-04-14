// US-008 core: Claude CLI subprocess wrapper.
#![allow(dead_code)]
///
/// Handles all Claude CLI invocations across every pipeline phase:
///   - Decompose: new session, returns session_id
///   - Execute / GapFix: --resume {session_id}
///   - PR: --resume {session_id}
///
/// All calls use:
///   --dangerously-skip-permissions  (allow file reads/writes)
///   --output-format json            (structured response)
///   --json-schema {schema}          (enforced output format)
///
/// Prompt is written to claude's stdin (avoids CLI arg length limits).
/// session_id and structured_output are extracted from the JSON response.
/// Budget exhaustion detected via response["subtype"] == "error_max_budget_usd".
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

// ---------------------------------------------------------------------------
// JSON schemas (compact — no whitespace; passed as single --json-schema arg)
// ---------------------------------------------------------------------------

pub const DECOMPOSE_SCHEMA: &str = concat!(
    r#"{"type":"object","required":["stories","session_note"],"properties":{"stories":{"#,
    r#""type":"array","items":{"type":"object","required":["id","title","depends_on"],"#,
    r#""properties":{"id":{"type":"string"},"title":{"type":"string"},"depends_on":{"#,
    r#""type":"array","items":{"type":"string"}}}}},"session_note":{"type":"string"}}}"#
);

pub const EXECUTE_SCHEMA: &str = concat!(
    r#"{"type":"object","required":["story_id","status","files_changed","tests_run","#,
    r#""test_passed","handoff_note","failure_type","progress_update"],"properties":{"#,
    r#""story_id":{"type":"string"},"status":{"type":"string","enum":["done","blocked"]},"#,
    r#""blocker":{"type":["string","null"]},"files_changed":{"type":"array","items":{"#,
    r#""type":"string"}},"tests_run":{"type":"array","items":{"type":"string"}},"#,
    r#""test_passed":{"type":"boolean"},"handoff_note":{"type":"string"},"failure_type":"#,
    r#"{"type":"string","enum":["none","test_failure","compilation_error","timeout","#,
    r#""budget_exhausted","unknown"]},"progress_update":{"type":"string"}}}"#
);

pub const PR_SCHEMA: &str = concat!(
    r#"{"type":"object","required":["pr_url","commits"],"properties":{"#,
    r#""pr_url":{"type":"string"},"commits":{"type":"array","items":{"type":"string"}}}}"#
);

// ---------------------------------------------------------------------------
// Response types
// ---------------------------------------------------------------------------

/// Raw top-level JSON response from `claude --output-format json`.
#[derive(Debug, Deserialize)]
struct RawResponse {
    #[serde(rename = "type")]
    response_type: Option<String>,
    subtype: Option<String>,
    session_id: Option<String>,
    result: Option<String>,
    structured_output: Option<Value>,
    total_cost_usd: Option<f64>,
}

/// Successful Claude output — all fields validated to be present.
#[derive(Debug)]
pub struct ClaudeOutput {
    /// Session ID for `--resume` in subsequent calls.
    pub session_id: String,
    /// Schema-enforced JSON from `--json-schema`.
    pub structured_output: Value,
    /// Accumulated cost for this call in USD.
    pub total_cost_usd: f64,
    /// Free-text result summary (the `result` field).
    pub result_text: String,
}

/// Result of a Claude CLI invocation.
#[derive(Debug)]
pub enum ClaudeResult {
    Success(ClaudeOutput),
    /// Claude hit the budget cap before committing — no changes were made.
    BudgetExhausted,
    /// The session ID passed via `--resume` is no longer valid (expired or not found).
    /// Caller should start a fresh session and prepend handoff notes.
    SessionExpired,
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

pub struct ClaudeRunner {
    /// Per-story budget cap in USD.
    pub max_budget_usd: f64,
}

impl ClaudeRunner {
    pub fn new(max_budget_usd: f64) -> Self {
        Self { max_budget_usd }
    }

    /// Decompose phase — new session. Budget capped at $1 (decompose is cheaper).
    pub fn run_decompose(&self, worktree: &Path, prompt: &str) -> Result<ClaudeResult> {
        self.call(worktree, prompt, None, self.max_budget_usd.min(1.00), DECOMPOSE_SCHEMA)
    }

    /// Execute phase — resume existing session.
    pub fn run_execute(&self, worktree: &Path, prompt: &str, session_id: &str) -> Result<ClaudeResult> {
        self.call(worktree, prompt, Some(session_id), self.max_budget_usd, EXECUTE_SCHEMA)
    }

    /// Execute phase — new session (used after Ralph loop clears session_id).
    pub fn run_execute_fresh(&self, worktree: &Path, prompt: &str) -> Result<ClaudeResult> {
        self.call(worktree, prompt, None, self.max_budget_usd, EXECUTE_SCHEMA)
    }

    /// GapFix phase — resume session with reviewer feedback appended.
    pub fn run_gapfix(&self, worktree: &Path, prompt: &str, session_id: &str) -> Result<ClaudeResult> {
        self.call(worktree, prompt, Some(session_id), self.max_budget_usd, EXECUTE_SCHEMA)
    }

    /// PR phase — push branch and open draft PR.
    pub fn run_pr(&self, worktree: &Path, prompt: &str, session_id: &str) -> Result<ClaudeResult> {
        self.call(worktree, prompt, Some(session_id), 1.00, PR_SCHEMA)
    }

    fn call(
        &self,
        worktree: &Path,
        prompt: &str,
        session_id: Option<&str>,
        budget_usd: f64,
        schema: &str,
    ) -> Result<ClaudeResult> {
        let mut args: Vec<String> = vec![
            "-p".into(),
            "--dangerously-skip-permissions".into(),
            "--output-format".into(),
            "json".into(),
            "--max-budget-usd".into(),
            format!("{:.2}", budget_usd),
            "--json-schema".into(),
            schema.into(),
        ];

        if let Some(sid) = session_id {
            args.push("--resume".into());
            args.push(sid.into());
        }

        let mut child = Command::new("claude")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .current_dir(worktree)
            .spawn()
            .context("Failed to spawn claude — is claude CLI installed and on PATH?")?;

        // Write prompt to stdin, then close the pipe so claude sees EOF.
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).context("Failed to write prompt to claude stdin")?;
            // stdin drop closes the pipe
        }

        let output = child.wait_with_output().context("Failed to wait for claude process")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Budget exhaustion: typically exit code 1.
        // Check JSON subtype field before treating non-zero exit as a hard error.
        if !output.status.success() {
            if let Ok(raw) = serde_json::from_str::<RawResponse>(&stdout) {
                if raw.subtype.as_deref() == Some("error_max_budget_usd") {
                    return Ok(ClaudeResult::BudgetExhausted);
                }
            }
            // Expired session detection (POC-1): stderr contains "No conversation found with session ID"
            if crate::session::is_expired_session_error(stderr.as_ref()) {
                return Ok(ClaudeResult::SessionExpired);
            }
            bail!(
                "claude exited {}\nstderr: {}\nstdout (first 500 chars): {}",
                output.status,
                stderr.trim(),
                &stdout[..stdout.len().min(500)]
            );
        }

        let raw: RawResponse = serde_json::from_str(&stdout).with_context(|| {
            format!(
                "claude returned non-JSON output: {}",
                &stdout[..stdout.len().min(300)]
            )
        })?;

        if raw.subtype.as_deref() == Some("error_max_budget_usd") {
            return Ok(ClaudeResult::BudgetExhausted);
        }

        Ok(ClaudeResult::Success(ClaudeOutput {
            session_id: raw.session_id.unwrap_or_default(),
            structured_output: raw.structured_output.unwrap_or(Value::Null),
            total_cost_usd: raw.total_cost_usd.unwrap_or(0.0),
            result_text: raw.result.unwrap_or_default(),
        }))
    }
}

// ---------------------------------------------------------------------------
// Helpers for parsing structured_output fields
// ---------------------------------------------------------------------------

/// Extract a required string field from structured_output.
pub fn get_str<'a>(output: &'a Value, field: &str) -> Result<&'a str> {
    output[field]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Missing '{}' in Claude structured output", field))
}

/// Extract a required boolean field from structured_output.
pub fn get_bool(output: &Value, field: &str) -> Result<bool> {
    output[field]
        .as_bool()
        .ok_or_else(|| anyhow::anyhow!("Missing '{}' in Claude structured output", field))
}

/// Extract a string array field from structured_output (returns empty vec if absent).
pub fn get_str_array(output: &Value, field: &str) -> Vec<String> {
    output[field]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_get_str_present() {
        let v = json!({"status": "done"});
        assert_eq!(get_str(&v, "status").unwrap(), "done");
    }

    #[test]
    fn test_get_str_missing() {
        let v = json!({});
        assert!(get_str(&v, "status").is_err());
    }

    #[test]
    fn test_get_bool_present() {
        let v = json!({"test_passed": true});
        assert!(get_bool(&v, "test_passed").unwrap());
    }

    #[test]
    fn test_get_str_array_present() {
        let v = json!({"files_changed": ["a.rs", "b.rs"]});
        assert_eq!(get_str_array(&v, "files_changed"), vec!["a.rs", "b.rs"]);
    }

    #[test]
    fn test_get_str_array_absent() {
        let v = json!({});
        assert!(get_str_array(&v, "files_changed").is_empty());
    }

    #[test]
    fn test_schema_constants_are_valid_json() {
        serde_json::from_str::<Value>(DECOMPOSE_SCHEMA).expect("DECOMPOSE_SCHEMA invalid JSON");
        serde_json::from_str::<Value>(EXECUTE_SCHEMA).expect("EXECUTE_SCHEMA invalid JSON");
        serde_json::from_str::<Value>(PR_SCHEMA).expect("PR_SCHEMA invalid JSON");
    }

    #[test]
    fn test_decompose_schema_has_required_fields() {
        let v: Value = serde_json::from_str(DECOMPOSE_SCHEMA).unwrap();
        let required = v["required"].as_array().unwrap();
        let fields: Vec<&str> = required.iter().filter_map(|f| f.as_str()).collect();
        assert!(fields.contains(&"stories"));
        assert!(fields.contains(&"session_note"));
    }

    #[test]
    fn test_execute_schema_has_required_fields() {
        let v: Value = serde_json::from_str(EXECUTE_SCHEMA).unwrap();
        let required = v["required"].as_array().unwrap();
        let fields: Vec<&str> = required.iter().filter_map(|f| f.as_str()).collect();
        for f in &["story_id", "status", "files_changed", "test_passed", "handoff_note", "failure_type", "progress_update"] {
            assert!(fields.contains(f), "Execute schema missing: {}", f);
        }
    }
}
