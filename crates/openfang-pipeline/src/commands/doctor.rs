/// US-000: Startup auth checks — Claude CLI, Backlog API, GitHub CLI.
///
/// All three are checked and reported. Pipeline stops if any fail.
use anyhow::Result;
use colored::Colorize;
use std::process::Command;

#[derive(Debug)]
pub struct DoctorResult {
    pub claude_ok: bool,
    pub claude_mode: String,
    pub claude_error: Option<String>,

    pub backlog_ok: bool,
    pub backlog_error: Option<String>,

    pub gh_ok: bool,
    pub gh_error: Option<String>,
}

impl DoctorResult {
    pub fn all_ok(&self) -> bool {
        self.claude_ok && self.backlog_ok && self.gh_ok
    }
}

/// Run all three checks. Returns the combined result.
///
/// `backlog_base` and `backlog_api_key` may be empty — if so, the Backlog check is skipped
/// with a warning (allows `pipeline doctor` without full config).
pub fn run(backlog_base: &str, backlog_api_key: &str) -> DoctorResult {
    let (claude_ok, claude_mode, claude_error) = check_claude();
    let (backlog_ok, backlog_error) = check_backlog(backlog_base, backlog_api_key);
    let (gh_ok, gh_error) = check_gh();

    DoctorResult {
        claude_ok,
        claude_mode,
        claude_error,
        backlog_ok,
        backlog_error,
        gh_ok,
        gh_error,
    }
}

/// Print the doctor report to stdout and return whether all checks passed.
pub fn print_and_check(result: &DoctorResult) -> bool {
    println!("\n{}", "Pipeline Doctor".bold());
    println!("{}", "─".repeat(40));

    // Claude CLI
    if result.claude_ok {
        println!(
            "  {} Claude CLI    auth ok ({})",
            "✓".green(),
            result.claude_mode.dimmed()
        );
    } else {
        println!("  {} Claude CLI    {}", "✗".red(), "not authenticated".red());
        if let Some(ref err) = result.claude_error {
            println!("    {}", err.dimmed());
        }
        println!(
            "\n  Fix:\n    claude setup-token          (recommended -- long-lived, subscription auth)\n    export ANTHROPIC_API_KEY=..  (API key alternative)"
        );
    }

    // Backlog API
    if result.backlog_ok {
        println!("  {} Backlog API   reachable", "✓".green());
    } else {
        let err = result.backlog_error.as_deref().unwrap_or("unknown error");
        if err.contains("not configured") {
            println!(
                "  {} Backlog API   {}",
                "⚠".yellow(),
                "not configured (set backlog_base + BACKLOG_API_KEY)".yellow()
            );
        } else {
            println!("  {} Backlog API   {}", "✗".red(), err.red());
            println!(
                "  {}  Check BACKLOG_API_KEY env var and backlog_base in .pipeline/config.toml",
                "Fix:".yellow()
            );
        }
    }

    // GitHub CLI
    if result.gh_ok {
        println!("  {} GitHub CLI    authenticated", "✓".green());
    } else {
        println!("  {} GitHub CLI    {}", "✗".red(), "not authenticated".red());
        if let Some(ref err) = result.gh_error {
            println!("    {}", err.dimmed());
        }
        println!("  {}  Run: gh auth login", "Fix:".yellow());
    }

    println!("{}", "─".repeat(40));

    if result.all_ok() {
        println!("  {} All checks passed\n", "✓".green().bold());
        true
    } else {
        println!("  {} One or more checks failed — pipeline cannot start\n", "✗".red().bold());
        false
    }
}

// ---------------------------------------------------------------------------
// Individual checks
// ---------------------------------------------------------------------------

fn check_claude() -> (bool, String, Option<String>) {
    // Auth probe: very cheap call, $0.001 budget cap
    let out = Command::new("claude")
        .args([
            "-p",
            "--max-budget-usd",
            "0.001",
            "Reply with the single word: ready",
        ])
        .env_remove("ANTHROPIC_API_KEY") // prefer stored token; API key users set it explicitly
        .output();

    match out {
        Err(e) => {
            (false, String::new(), Some(format!("claude binary not found or not executable: {e}")))
        }
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_lowercase();
            let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
            let combined = format!("{} {}", stdout, stderr);

            if is_auth_error(&combined) {
                return (
                    false,
                    String::new(),
                    Some("Authentication failed -- no valid token or API key".to_string()),
                );
            }

            if output.status.success() || stdout.contains("ready") {
                // Detect which auth mode is active
                let mode = detect_auth_mode();
                return (true, mode, None);
            }

            // Non-auth error (network, model, etc.) — treat as auth unknown
            (
                false,
                String::new(),
                Some(format!("Unexpected error: {}", stderr.trim())),
            )
        }
    }
}

fn is_auth_error(output: &str) -> bool {
    output.contains("authentication")
        || output.contains("not authenticated")
        || output.contains("api key")
        || output.contains("unauthorized")
        || output.contains("credit balance")
        || output.contains("x-api-key")
        || output.contains("login")
        || output.contains("sign in")
}

fn detect_auth_mode() -> String {
    if std::env::var("ANTHROPIC_API_KEY").is_ok() {
        "ANTHROPIC_API_KEY".to_string()
    } else {
        "setup-token".to_string()
    }
}

fn check_backlog(backlog_base: &str, api_key: &str) -> (bool, Option<String>) {
    if backlog_base.is_empty() || api_key.is_empty() {
        return (false, Some("not configured".to_string()));
    }

    let url = format!("{}/api/v2/projects?apiKey={}&count=1", backlog_base, api_key);

    let out = Command::new("curl")
        .args(["-sf", "--connect-timeout", "10", "--max-time", "15", &url])
        .output();

    match out {
        Err(e) => (false, Some(format!("curl failed: {e}"))),
        Ok(output) => {
            if output.status.success() {
                (true, None)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                (false, Some(if stderr.is_empty() { "HTTP error or unreachable".to_string() } else { stderr }))
            }
        }
    }
}

fn check_gh() -> (bool, Option<String>) {
    let out = Command::new("gh")
        .args(["auth", "status"])
        .output();

    match out {
        Err(e) => (false, Some(format!("gh binary not found: {e}"))),
        Ok(output) => {
            if output.status.success() {
                (true, None)
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                (false, Some(stderr))
            }
        }
    }
}

// ---------------------------------------------------------------------------

pub fn run_and_exit_on_failure(backlog_base: &str, backlog_api_key: &str) -> Result<()> {
    let result = run(backlog_base, backlog_api_key);
    let ok = print_and_check(&result);
    if !ok {
        std::process::exit(1);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_auth_error_detects_keywords() {
        assert!(is_auth_error("authentication required"));
        assert!(is_auth_error("not authenticated"));
        assert!(is_auth_error("invalid api key provided"));
        assert!(is_auth_error("401 unauthorized"));
        assert!(is_auth_error("your credit balance is exhausted"));
        assert!(is_auth_error("missing x-api-key header"));
        assert!(is_auth_error("please login to continue"));
        assert!(is_auth_error("sign in with your account"));
    }

    #[test]
    fn test_is_auth_error_passes_normal_output() {
        assert!(!is_auth_error("ready"));
        assert!(!is_auth_error("hello world"));
        assert!(!is_auth_error("generating response..."));
        assert!(!is_auth_error(""));
        assert!(!is_auth_error("claude version 1.0.0"));
    }

    #[test]
    fn test_doctor_result_all_ok_when_all_pass() {
        let r = DoctorResult {
            claude_ok: true,
            claude_mode: "setup-token".into(),
            claude_error: None,
            backlog_ok: true,
            backlog_error: None,
            gh_ok: true,
            gh_error: None,
        };
        assert!(r.all_ok());
    }

    #[test]
    fn test_doctor_result_not_ok_if_claude_fails() {
        let r = DoctorResult {
            claude_ok: false,
            claude_mode: String::new(),
            claude_error: Some("not authenticated".into()),
            backlog_ok: true,
            backlog_error: None,
            gh_ok: true,
            gh_error: None,
        };
        assert!(!r.all_ok());
    }

    #[test]
    fn test_doctor_result_not_ok_if_backlog_fails() {
        let r = DoctorResult {
            claude_ok: true,
            claude_mode: "setup-token".into(),
            claude_error: None,
            backlog_ok: false,
            backlog_error: Some("not configured".into()),
            gh_ok: true,
            gh_error: None,
        };
        assert!(!r.all_ok());
    }

    #[test]
    fn test_doctor_result_not_ok_if_gh_fails() {
        let r = DoctorResult {
            claude_ok: true,
            claude_mode: "setup-token".into(),
            claude_error: None,
            backlog_ok: true,
            backlog_error: None,
            gh_ok: false,
            gh_error: Some("not authenticated".into()),
        };
        assert!(!r.all_ok());
    }
}
