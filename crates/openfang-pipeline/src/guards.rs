// US-010/011: Guard runner — grep-based pattern checks on changed files.
#![allow(dead_code)]
///
/// Rules are loaded from `.pipeline/guards.toml`. Each rule is run against
/// only the files Claude reported as changed. Results are sorted: errors first.
///
/// Special rule pattern "LINE_COUNT_CHECK": counts lines in handler functions
/// in `routes.rs` files. All other patterns are treated as extended regex.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warn,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuardViolation {
    pub rule: String,
    pub severity: Severity,
    /// Repo-relative file path.
    pub file: String,
    pub line: u32,
    pub snippet: String,
}

#[derive(Debug, Deserialize)]
struct GuardRule {
    name: String,
    severity: Severity,
    #[serde(default = "bool_true")]
    enabled: bool,
    #[serde(default)]
    description: String,
    pattern: String,
    #[serde(default)]
    exclude_patterns: Vec<String>,
    #[serde(default)]
    awk_check: bool,
    #[serde(default = "default_max_lines")]
    max_lines: usize,
}

fn bool_true() -> bool { true }
fn default_max_lines() -> usize { 30 }

#[derive(Debug, Default, Deserialize)]
struct GuardsConfig {
    #[serde(rename = "rule", default)]
    rules: Vec<GuardRule>,
}

// ---------------------------------------------------------------------------
// Runner
// ---------------------------------------------------------------------------

pub struct GuardRunner {
    rules: Vec<GuardRule>,
}

impl GuardRunner {
    /// Load rules from `guards.toml`. Returns an empty runner if file absent.
    pub fn load(guards_toml: &Path) -> Result<Self> {
        let rules = if guards_toml.exists() {
            let content = std::fs::read_to_string(guards_toml)
                .with_context(|| format!("Failed to read {}", guards_toml.display()))?;
            let cfg: GuardsConfig = toml::from_str(&content)
                .with_context(|| format!("Failed to parse {}", guards_toml.display()))?;
            cfg.rules
        } else {
            Vec::new()
        };
        Ok(Self { rules })
    }

    /// Run all enabled rules against `files`. Paths are relative to `repo_root`.
    /// Returns violations sorted by severity (errors first).
    pub fn run(&self, files: &[String], repo_root: &Path) -> Vec<GuardViolation> {
        let mut violations: Vec<GuardViolation> = Vec::new();

        for file_rel in files {
            let abs = repo_root.join(file_rel);
            if !abs.exists() {
                continue;
            }

            for rule in &self.rules {
                if !rule.enabled {
                    continue;
                }
                if rule.pattern == "LINE_COUNT_CHECK" {
                    violations.extend(check_handler_lines(file_rel, &abs, rule));
                } else {
                    violations.extend(run_grep_rule(file_rel, &abs, rule));
                }
            }
        }

        // errors before warnings
        violations.sort_by_key(|v| if v.severity == Severity::Error { 0u8 } else { 1u8 });
        violations
    }

    /// Number of rules loaded.
    pub fn rule_count(&self) -> usize {
        self.rules.len()
    }
}

// ---------------------------------------------------------------------------
// Grep-based rule
// ---------------------------------------------------------------------------

fn run_grep_rule(file_rel: &str, abs: &Path, rule: &GuardRule) -> Vec<GuardViolation> {
    let out = Command::new("grep")
        .args(["-En", &rule.pattern, abs.to_str().unwrap_or("")])
        .output();

    let output = match out {
        Ok(o) => o,
        Err(_) => return vec![], // grep not available — skip silently
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut violations = Vec::new();

    for line in stdout.lines() {
        // grep -n output format: "<line_num>:<content>"
        let (line_num, content) = match line.find(':') {
            Some(pos) => {
                let num: u32 = line[..pos].parse().unwrap_or(0);
                (num, &line[pos + 1..])
            }
            None => (0, line),
        };

        // Apply exclude patterns — skip if any matches
        let excluded = rule
            .exclude_patterns
            .iter()
            .any(|pat| content.contains(pat.as_str()) || file_rel.contains(pat.as_str()));

        if !excluded {
            violations.push(GuardViolation {
                rule: rule.name.clone(),
                severity: rule.severity.clone(),
                file: file_rel.to_string(),
                line: line_num,
                snippet: content.trim().to_string(),
            });
        }
    }

    violations
}

// ---------------------------------------------------------------------------
// LINE_COUNT_CHECK: flag handler functions > max_lines in routes.rs files
// ---------------------------------------------------------------------------

fn check_handler_lines(file_rel: &str, abs: &Path, rule: &GuardRule) -> Vec<GuardViolation> {
    // Only applies to routes.rs files
    if !file_rel.ends_with("routes.rs") {
        return vec![];
    }

    let content = match std::fs::read_to_string(abs) {
        Ok(c) => c,
        Err(_) => return vec![],
    };

    let mut violations = Vec::new();
    let mut fn_start: Option<(u32, String)> = None; // (line_number, fn_name)
    let mut brace_depth: i32 = 0;
    let mut body_lines: u32 = 0;
    let mut in_fn_body = false;

    for (idx, line) in content.lines().enumerate() {
        let line_num = (idx + 1) as u32;
        let trimmed = line.trim();

        // Detect function start — simple heuristic: line contains "fn " and ends with "{"
        // or is just the signature (next line opens brace)
        if fn_start.is_none() && trimmed.contains("fn ") && trimmed.contains('(') {
            let name = extract_fn_name(trimmed);
            fn_start = Some((line_num, name));
            brace_depth = 0;
            body_lines = 0;
            in_fn_body = false;
        }

        if fn_start.is_some() {
            // Count braces to track function body boundaries
            for ch in trimmed.chars() {
                match ch {
                    '{' => {
                        brace_depth += 1;
                        if brace_depth == 1 {
                            in_fn_body = true;
                        }
                    }
                    '}' => {
                        brace_depth -= 1;
                    }
                    _ => {}
                }
            }

            if in_fn_body && brace_depth > 0 && !trimmed.is_empty() {
                body_lines += 1;
            }

            // Function closed
            if in_fn_body && brace_depth == 0 {
                if body_lines > rule.max_lines as u32 {
                    let (start_line, fn_name) = fn_start.take().unwrap();
                    violations.push(GuardViolation {
                        rule: rule.name.clone(),
                        severity: rule.severity.clone(),
                        file: file_rel.to_string(),
                        line: start_line,
                        snippet: format!(
                            "fn {} has {} lines (max {})",
                            fn_name, body_lines, rule.max_lines
                        ),
                    });
                } else {
                    fn_start = None;
                }
                body_lines = 0;
                in_fn_body = false;
            }
        }
    }

    violations
}

fn extract_fn_name(line: &str) -> String {
    // Extract function name from "pub async fn foo(...)" etc.
    if let Some(fn_pos) = line.find("fn ") {
        let after = &line[fn_pos + 3..];
        let end = after.find(|c: char| !c.is_alphanumeric() && c != '_').unwrap_or(after.len());
        return after[..end].to_string();
    }
    "unknown".to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_rule_toml(dir: &TempDir, content: &str) -> std::path::PathBuf {
        let path = dir.path().join("guards.toml");
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn test_load_empty_when_file_absent() {
        let dir = TempDir::new().unwrap();
        let runner = GuardRunner::load(&dir.path().join("guards.toml")).unwrap();
        assert_eq!(runner.rule_count(), 0);
    }

    #[test]
    fn test_load_rules_from_toml() {
        let dir = TempDir::new().unwrap();
        let toml = "[[rule]]\nname=\"test_rule\"\nseverity=\"error\"\nenabled=true\ndescription=\"test\"\npattern='unwrap'\n";
        let path = write_rule_toml(&dir, toml);
        let runner = GuardRunner::load(&path).unwrap();
        assert_eq!(runner.rule_count(), 1);
    }

    #[test]
    fn test_grep_rule_detects_violation() {
        let dir = TempDir::new().unwrap();
        // File with an unwrap call
        let src = dir.path().join("lib.rs");
        std::fs::write(&src, "fn main() {\n    foo().unwrap();\n}\n").unwrap();

        let toml = "[[rule]]\nname=\"no_unwrap\"\nseverity=\"warn\"\nenabled=true\ndescription=\"\"\npattern='\\.unwrap\\(\\)'\n";
        let path = write_rule_toml(&dir, toml);
        let runner = GuardRunner::load(&path).unwrap();

        let violations = runner.run(&["lib.rs".to_string()], dir.path());
        assert_eq!(violations.len(), 1);
        assert_eq!(violations[0].rule, "no_unwrap");
        assert_eq!(violations[0].line, 2);
    }

    #[test]
    fn test_grep_rule_exclude_pattern() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn main() {\n    foo().unwrap();\n}\n").unwrap();

        // Exclude files containing "lib" in name
        let toml = "[[rule]]\nname=\"no_unwrap\"\nseverity=\"warn\"\nenabled=true\ndescription=\"\"\npattern='\\.unwrap\\(\\)'\nexclude_patterns=[\"lib\"]\n";
        let path = write_rule_toml(&dir, toml);
        let runner = GuardRunner::load(&path).unwrap();

        let violations = runner.run(&["lib.rs".to_string()], dir.path());
        assert_eq!(violations.len(), 0);
    }

    #[test]
    fn test_disabled_rule_skipped() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn main() {\n    foo().unwrap();\n}\n").unwrap();

        let toml = "[[rule]]\nname=\"no_unwrap\"\nseverity=\"warn\"\nenabled=false\ndescription=\"\"\npattern='\\.unwrap\\(\\)'\n";
        let path = write_rule_toml(&dir, toml);
        let runner = GuardRunner::load(&path).unwrap();

        let violations = runner.run(&["lib.rs".to_string()], dir.path());
        assert_eq!(violations.len(), 0);
    }

    #[test]
    fn test_violations_sorted_errors_first() {
        let dir = TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "secret = \"hardcoded\"\nfoo().unwrap();\n",
        ).unwrap();

        let toml = concat!(
            "[[rule]]\nname=\"warn_rule\"\nseverity=\"warn\"\nenabled=true\ndescription=\"\"\npattern='unwrap'\n\n",
            "[[rule]]\nname=\"error_rule\"\nseverity=\"error\"\nenabled=true\ndescription=\"\"\npattern='secret'\n",
        );
        let path = write_rule_toml(&dir, toml);
        let runner = GuardRunner::load(&path).unwrap();

        let violations = runner.run(&["lib.rs".to_string()], dir.path());
        assert!(!violations.is_empty());
        assert_eq!(violations[0].severity, Severity::Error);
    }

    #[test]
    fn test_nonexistent_file_skipped() {
        let dir = TempDir::new().unwrap();
        let toml = "[[rule]]\nname=\"r\"\nseverity=\"error\"\nenabled=true\ndescription=\"\"\npattern='x'\n";
        let path = write_rule_toml(&dir, toml);
        let runner = GuardRunner::load(&path).unwrap();

        // File does not exist — must not panic, must return 0 violations
        let violations = runner.run(&["ghost.rs".to_string()], dir.path());
        assert_eq!(violations.len(), 0);
    }

    #[test]
    fn test_extract_fn_name() {
        assert_eq!(extract_fn_name("pub async fn handle_request("), "handle_request");
        assert_eq!(extract_fn_name("fn foo()"), "foo");
        assert_eq!(extract_fn_name("no function here"), "unknown");
    }
}
