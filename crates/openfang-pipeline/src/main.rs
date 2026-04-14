/// openfang-pipeline — Autonomous software development pipeline CLI.
///
/// Usage:
///   pipeline setup              Bootstrap .pipeline/ for this repo
///   pipeline setup --refresh    Regenerate role files from current codebase
///   pipeline doctor             Check Claude CLI, Backlog API, and gh auth
///   pipeline run <ISSUE-KEY>    Run the pipeline for one Backlog issue
///   pipeline resume <ISSUE-KEY> Resume a paused or interrupted issue
///   pipeline status             Show current pipeline status
///   pipeline logs               Tail pipeline logs (daemon mode)
use clap::{Parser, Subcommand};
use colored::Colorize;
use std::path::PathBuf;

mod commands;
mod config;
mod git;
mod prompt;
mod state;

#[derive(Parser)]
#[command(
    name = "pipeline",
    version,
    about = "Autonomous software development pipeline",
    long_about = "Reads Backlog issues, uses Claude CLI to implement them, posts PRs for human review."
)]
struct Cli {
    /// Repository root (default: current directory).
    #[arg(long, global = true, value_name = "PATH")]
    repo: Option<PathBuf>,

    #[command(subcommand)]
    command: PipelineCommand,
}

#[derive(Subcommand)]
enum PipelineCommand {
    /// Bootstrap .pipeline/ config for this repo (run once per repo).
    Setup {
        /// Skip CLAUDE.md check.
        #[arg(long)]
        force: bool,
        /// Regenerate backend.md and frontend.md from current codebase.
        #[arg(long)]
        refresh: bool,
    },

    /// Check Claude CLI auth, Backlog API, and GitHub CLI (run before first issue).
    Doctor,

    /// Run the pipeline for a specific Backlog issue.
    Run {
        /// Backlog issue key (e.g. OFANG-123).
        issue_key: String,
    },

    /// Resume a paused or interrupted pipeline run.
    Resume {
        /// Backlog issue key.
        issue_key: String,
    },

    /// Show current pipeline status (active issue, stories, cost).
    Status,

    /// Tail pipeline logs (daemon mode).
    Logs {
        /// Number of lines to show.
        #[arg(short = 'n', default_value = "50")]
        lines: usize,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    let repo_root = cli
        .repo
        .unwrap_or_else(|| std::env::current_dir().expect("Failed to get current directory"));

    if let Err(e) = run(cli.command, repo_root).await {
        eprintln!("\n{} {}\n", "Error:".red().bold(), e);
        std::process::exit(1);
    }
}

async fn run(command: PipelineCommand, repo_root: PathBuf) -> anyhow::Result<()> {
    match command {
        PipelineCommand::Setup { force, refresh } => {
            commands::setup::run(commands::setup::SetupArgs {
                force,
                refresh,
                repo_root,
            })?;
        }

        PipelineCommand::Doctor => {
            // Load config if available for Backlog URL; fall back to empty
            let (backlog_base, backlog_api_key) = load_backlog_config(&repo_root);
            commands::doctor::run_and_exit_on_failure(&backlog_base, &backlog_api_key)?;
        }

        PipelineCommand::Run { issue_key } => {
            run_issue(&repo_root, &issue_key).await?;
        }

        PipelineCommand::Resume { issue_key } => {
            resume_issue(&repo_root, &issue_key).await?;
        }

        PipelineCommand::Status => {
            show_status(&repo_root)?;
        }

        PipelineCommand::Logs { lines } => {
            show_logs(lines)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_backlog_config(repo_root: &std::path::Path) -> (String, String) {
    let backlog_base = config::PipelineConfig::load(repo_root)
        .map(|c| c.backlog_base)
        .unwrap_or_default();
    let api_key = std::env::var("BACKLOG_API_KEY").unwrap_or_default();
    (backlog_base, api_key)
}

async fn run_issue(repo_root: &std::path::Path, issue_key: &str) -> anyhow::Result<()> {
    println!("\n{} {}", "Pipeline →".bold(), issue_key.cyan().bold());
    println!("{}", "─".repeat(50));

    let cfg = config::PipelineConfig::load(repo_root)?;

    // Startup checks
    let backlog_api_key = std::env::var("BACKLOG_API_KEY").unwrap_or_default();
    let doctor = commands::doctor::run(&cfg.backlog_base, &backlog_api_key);
    if !commands::doctor::print_and_check(&doctor) {
        std::process::exit(1);
    }

    // Branch + worktree setup (US-003)
    println!("\n{}", "Branch Setup".bold());
    let (branch, worktree_path) =
        git::setup_issue_workspace(repo_root, issue_key, &cfg.base_branch)?;
    println!("  Branch:   {}", branch.cyan());
    println!("  Worktree: {}", worktree_path.display());

    println!(
        "\n  {} Issue {} ready — run `pipeline resume {}` to continue after interruption",
        "✓".green(),
        issue_key.cyan(),
        issue_key
    );
    println!(
        "\n  {} Full pipeline execution (decompose → implement → gate → PR) coming in Phase 2\n",
        "ℹ".cyan()
    );

    Ok(())
}

async fn resume_issue(repo_root: &std::path::Path, issue_key: &str) -> anyhow::Result<()> {
    if !state::PipelineState::exists(repo_root, issue_key) {
        anyhow::bail!(
            "No pipeline state found for {}. Run `pipeline run {}` first.",
            issue_key,
            issue_key
        );
    }

    let state = state::PipelineState::load(repo_root, issue_key)?;
    println!(
        "\n{} {} (resuming from {:?})",
        "Pipeline →".bold(),
        issue_key.cyan().bold(),
        state.phase
    );

    // TODO: Phase 2 — wire into full execution loop
    println!(
        "  {} Resume logic (full execution loop) coming in Phase 2\n",
        "ℹ".cyan()
    );

    Ok(())
}

fn show_status(repo_root: &std::path::Path) -> anyhow::Result<()> {
    // Find any active STATE files
    let pipeline_dir = repo_root.join("PIPELINE");
    if !pipeline_dir.exists() {
        println!("\n  No pipeline running\n");
        return Ok(());
    }

    let entries = std::fs::read_dir(&pipeline_dir)?;
    let mut found = false;

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("STATE-") && name_str.ends_with(".json") {
            let raw = std::fs::read_to_string(entry.path())?;
            if let Ok(s) = serde_json::from_str::<serde_json::Value>(&raw) {
                let key = s["issue_key"].as_str().unwrap_or("?");
                let phase = s["phase"].as_str().unwrap_or("?");
                let cost = s["total_cost_usd"].as_f64().unwrap_or(0.0);
                let branch = s["branch"].as_str().unwrap_or("?");
                println!(
                    "  {} {} | Phase: {} | Branch: {} | Cost: ${:.2}",
                    "●".cyan(),
                    key.bold(),
                    phase,
                    branch,
                    cost
                );
                found = true;
            }
        }
    }

    if !found {
        println!("\n  No pipeline running\n");
    }

    Ok(())
}

fn show_logs(lines: usize) -> anyhow::Result<()> {
    let log_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(".pipeline")
        .join("logs");

    if !log_dir.exists() {
        println!("No pipeline logs found at {}", log_dir.display());
        return Ok(());
    }

    // Find most recent log file
    let mut entries: Vec<_> = std::fs::read_dir(&log_dir)?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .ends_with(".log")
        })
        .collect();

    entries.sort_by_key(|e| e.metadata().and_then(|m| m.modified()).ok());
    entries.reverse();

    if let Some(latest) = entries.first() {
        let content = std::fs::read_to_string(latest.path())?;
        let tail: Vec<&str> = content.lines().rev().take(lines).collect();
        for line in tail.iter().rev() {
            println!("{}", line);
        }
    } else {
        println!("No log files found in {}", log_dir.display());
    }

    Ok(())
}
