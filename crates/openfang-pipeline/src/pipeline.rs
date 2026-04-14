/// Full pipeline orchestration — US-006, US-007, US-008, US-009, US-012.
///
/// State machine:
///   Decompose → Gate1 → Execute → Guard → Gate2 → [next story | GapFix | Pr] → Complete
///
/// Each phase transition is persisted to PIPELINE/STATE-{key}.json before proceeding.
/// Crash recovery: re-running `pipeline run` or `pipeline resume` re-enters from the saved phase.
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::io::{self, BufRead};
use std::path::Path;

use crate::backlog::{BacklogClient, STATUS_IN_PROGRESS, STATUS_OPEN};
use crate::classifier::{classify, ClassifyResult};
use crate::config::PipelineConfig;
use crate::feedback;
use crate::gate::{GateClient, GateDecision, GateSummary};
use crate::guards::GuardRunner;
use crate::prompt::{self, IssueContext, PromptPhase, StoryContext};
use crate::runner::{self, ClaudeResult, ClaudeRunner};
use crate::session;
use crate::state::{Phase, PipelineState, Role, StoryState, StoryStatus};

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Run the pipeline for a single Backlog issue from scratch (or continue if state exists).
pub async fn run(repo_root: &Path, issue_key: &str, cfg: &PipelineConfig) -> Result<()> {
    let api_key = std::env::var("BACKLOG_API_KEY")
        .context("BACKLOG_API_KEY environment variable not set")?;

    if cfg.backlog_base.is_empty() {
        bail!("backlog_base not configured. Edit .pipeline/config.toml and set backlog_base.");
    }

    let backlog = BacklogClient::new(&cfg.backlog_base, &api_key);

    // Check for stale states from previous crashed runs
    warn_stale_states(repo_root, &backlog, cfg).await;

    println!("\n{} {}", "Pipeline →".bold(), issue_key.cyan().bold());
    println!("{}", "─".repeat(55));

    // Fetch issue
    println!("  Fetching issue {}...", issue_key);
    let issue = backlog.fetch_issue(issue_key).await
        .with_context(|| format!("Failed to fetch issue '{}' from Backlog", issue_key))?;
    println!("  {} {}: {}", "✓".green(), issue.key.bold(), issue.summary);

    // Classify role
    let role = match classify(&issue, &cfg.roles) {
        ClassifyResult::Single(r) => {
            let label = match r { Role::Backend => "backend", Role::Frontend => "frontend" };
            println!("  {} Role: {}", "✓".green(), label.cyan());
            r
        }
        ClassifyResult::Fullstack => {
            let msg = format!(
                "Pipeline skipped {}: fullstack issues not supported in v1. \
                Split into a backend issue and a frontend issue in Backlog.",
                issue_key
            );
            let _ = backlog.add_comment(issue_key, &msg).await;
            bail!("{}", msg);
        }
        ClassifyResult::Unclassified => {
            let msg = format!(
                "Pipeline skipped {}: issue has no backend or frontend label. \
                Add a role label to process this issue.",
                issue_key
            );
            let _ = backlog.add_comment(issue_key, &msg).await;
            bail!("{}", msg);
        }
    };

    let role_file = match role {
        Role::Backend => cfg.backend_file(repo_root),
        Role::Frontend => cfg.frontend_file(repo_root),
    };

    // Git workspace
    println!("\n{}", "Branch Setup".bold());
    let (branch, worktree_path) = crate::git::setup_issue_workspace(repo_root, issue_key, &cfg.base_branch)?;
    println!("  Branch:   {}", branch.cyan());
    println!("  Worktree: {}", worktree_path.display());

    // Create or load state
    let mut state = if PipelineState::exists(repo_root, issue_key) {
        println!("  {} Resuming saved state", "↩".yellow());
        PipelineState::load(repo_root, issue_key)?
    } else {
        PipelineState::new(issue_key, &issue.summary, role, &branch, worktree_path.clone())
    };

    // Mark In Progress in Backlog
    let _ = backlog.update_status(issue_key, STATUS_IN_PROGRESS).await;

    let issue_ctx = IssueContext {
        key: issue.key.clone(),
        summary: issue.summary.clone(),
        description: issue.description_text().to_string(),
        issue_type: issue.issue_type.name.clone(),
        priority: issue.priority.name.clone(),
    };

    // Validate session config before starting
    let max_stories_per_session =
        session::validate_max_stories_per_session(cfg.max_stories_per_session);

    let claude = ClaudeRunner::new(cfg.max_budget_usd);
    let guards = GuardRunner::load(&cfg.guards_file(repo_root)).unwrap_or_else(|e| {
        eprintln!("  {} Failed to load guards.toml: {} — skipping guards", "WARN".yellow(), e);
        GuardRunner::load(&std::path::PathBuf::from("/dev/null")).unwrap()
    });

    // -----------------------------------------------------------------------
    // State machine loop
    // -----------------------------------------------------------------------
    loop {
        state.save(repo_root)?;

        match state.phase {
            Phase::Decompose => {
                decompose(&mut state, repo_root, &worktree_path, &issue_ctx, &role_file, cfg, &claude)?;
            }

            Phase::Gate1 => {
                gate1(&mut state, repo_root, issue_key)?;
            }

            Phase::Execute => {
                execute(&mut state, repo_root, &worktree_path, &issue_ctx, &role_file, cfg, &claude)?;
            }

            Phase::Guard => {
                run_guards(&mut state, &guards, &worktree_path)?;
            }

            Phase::Gate2 => {
                gate2(&mut state, repo_root, cfg, &backlog, max_stories_per_session).await?;
            }

            Phase::GapFix => {
                gapfix(&mut state, repo_root, &worktree_path, &issue_ctx, &role_file, cfg, &claude)?;
            }

            Phase::Pr => {
                pr_phase(&mut state, repo_root, &worktree_path, &issue_ctx, &role_file, cfg, &claude, &backlog).await?;
            }

            Phase::Complete => {
                println!("\n  {} Pipeline complete for {}\n", "✓".green().bold(), issue_key.cyan());
                break;
            }

            Phase::Abandoned => {
                println!("\n  {} Pipeline abandoned for {}\n", "✗".red().bold(), issue_key.cyan());
                break;
            }
        }
    }

    Ok(())
}

/// Resume a pipeline that was interrupted — re-enters from saved phase.
pub async fn resume(repo_root: &Path, issue_key: &str, cfg: &PipelineConfig) -> Result<()> {
    if !PipelineState::exists(repo_root, issue_key) {
        bail!("No pipeline state found for {}. Run `pipeline run {}` first.", issue_key, issue_key);
    }
    // resume just calls run — the state machine picks up from wherever it left off
    run(repo_root, issue_key, cfg).await
}

// ---------------------------------------------------------------------------
// Phase implementations
// ---------------------------------------------------------------------------

fn decompose(
    state: &mut PipelineState,
    repo_root: &Path,
    worktree: &Path,
    issue_ctx: &IssueContext,
    role_file: &Path,
    cfg: &PipelineConfig,
    claude: &ClaudeRunner,
) -> Result<()> {
    println!("\n{}", "Decompose".bold());

    let mut assembled = prompt::assemble(repo_root, issue_ctx, role_file, PromptPhase::Decompose, None, cfg.repo_map_lines)?;

    // Append Gate1 rejection feedback to the prompt if present
    if let Some(fb) = state.pending_feedback.take() {
        assembled.text.push_str(&format!(
            "\n\n---\n\nPREVIOUS PLAN REJECTED BY REVIEWER:\n{}\n\nRevise the plan accordingly.",
            fb
        ));
    }

    println!("  Prompt: ~{} tokens", assembled.estimated_tokens);

    println!("  Calling Claude to decompose issue...");
    let result = claude.run_decompose(worktree, &assembled.text)?;

    let output = match result {
        ClaudeResult::BudgetExhausted => bail!("Budget exhausted during decompose — increase max_budget_usd"),
        ClaudeResult::SessionExpired => bail!("Unexpected session expiry during decompose phase"),
        ClaudeResult::Success(o) => o,
    };

    // Save session_id
    state.session_id = Some(output.session_id.clone());
    state.add_cost(output.total_cost_usd);

    // Parse story list from structured_output
    let stories_json = &output.structured_output["stories"];
    let session_note = output.structured_output["session_note"].as_str().unwrap_or("").to_string();

    let stories: Vec<StoryState> = stories_json
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("decompose output missing 'stories' array"))?
        .iter()
        .map(|s| StoryState {
            id: s["id"].as_str().unwrap_or("US-???").to_string(),
            title: s["title"].as_str().unwrap_or("").to_string(),
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
        })
        .collect();

    println!("  {} {} stories decomposed", "✓".green(), stories.len());
    for s in &stories {
        println!("    {} {}", "·".dimmed(), s.title);
    }

    state.set_stories(stories);
    // Store session_note in the first story (quick workaround — proper field in v2)
    let _ = session_note; // available to prompt assembly via progress.md
    state.phase = Phase::Gate1;

    Ok(())
}

fn gate1(state: &mut PipelineState, repo_root: &Path, issue_key: &str) -> Result<()> {
    let plan_path = repo_root.join("PIPELINE").join(format!("PLAN-{}.md", issue_key));

    println!("\n{}", "Gate 1 — Plan Review".bold());
    println!("{}", "─".repeat(55));
    println!("  Issue:  {} — {}", issue_key.cyan(), state.issue_summary);
    println!("  Stories: {}", state.stories.len());
    for s in &state.stories {
        println!("    {} {}: {}", "·".dimmed(), s.id.bold(), s.title);
    }
    if plan_path.exists() {
        println!("\n  Plan file: {}", plan_path.display());
    }
    println!();
    println!("  [A] Approve — proceed to implementation");
    println!("  [R] Reject  — send feedback to Claude");
    println!("  [Q] Quit    — pause pipeline");
    print!("\n  Choice: ");
    io::Write::flush(&mut io::stdout()).ok();

    let stdin = io::stdin();
    let line = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;
    let choice = line.trim().to_lowercase();

    match choice.as_str() {
        "a" | "approve" => {
            println!("  {} Plan approved\n", "✓".green());
            state.phase = Phase::Execute;
        }
        "r" | "reject" => {
            println!("  Enter feedback for Claude:");
            let feedback = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;
            if feedback.trim().is_empty() {
                bail!("Feedback required when rejecting the plan");
            }
            println!("  {} Re-decomposing with feedback...", "↻".yellow());
            state.pending_feedback = Some(feedback.trim().to_string());
            state.phase = Phase::Decompose;
            state.stories.clear();
            state.session_id = None;
        }
        "q" | "quit" => {
            println!("  {} Pipeline paused. Run `pipeline resume {}` to continue.", "⏸".yellow(), issue_key);
            std::process::exit(0);
        }
        _ => {
            println!("  Unknown choice '{}' — defaulting to quit", choice);
            std::process::exit(0);
        }
    }

    Ok(())
}

fn execute(
    state: &mut PipelineState,
    repo_root: &Path,
    worktree: &Path,
    issue_ctx: &IssueContext,
    role_file: &Path,
    cfg: &PipelineConfig,
    claude: &ClaudeRunner,
) -> Result<()> {
    let story = state.current_story()
        .ok_or_else(|| anyhow::anyhow!("No current story to execute"))?
        .clone();

    println!("\n{} {}: {}", "Execute".bold(), story.id.cyan(), story.title);

    let session_id = state.session_id.clone().unwrap_or_default();

    let story_ctx = StoryContext {
        id: story.id.clone(),
        title: story.title.clone(),
        completed_stories: state.completed_story_ids(),
        session_note: String::new(), // will be improved in v2
        feedback: None,
    };

    let mut assembled = prompt::assemble(
        repo_root,
        issue_ctx,
        role_file,
        PromptPhase::Execute,
        Some(&story_ctx),
        cfg.repo_map_lines,
    )?;

    // On fresh session: prepend handoff notes from all completed stories so Claude re-orients
    if session_id.is_empty() {
        let completed_ids = state.completed_story_ids();
        if !completed_ids.is_empty() {
            let notes = session::load_handoff_notes(repo_root, &issue_ctx.key, &completed_ids);
            let preamble = session::build_handoff_preamble(&notes);
            if !preamble.is_empty() {
                println!("  {} Injecting {} handoff notes for fresh session", "↻".cyan(), notes.len());
                assembled.text = format!("{}\n\n---\n\n{}", preamble, assembled.text);
            }
        }
    }

    println!("  Prompt: ~{} tokens", assembled.estimated_tokens);

    let phase_display = if session_id.is_empty() {
        "new session".to_string()
    } else {
        format!("--resume {}", &session_id[..session_id.len().min(8)])
    };
    println!("  Calling Claude ({})", phase_display.dimmed());

    // Check token threshold — proactively start fresh session if prompt is too large
    if assembled.estimated_tokens > session::TOKEN_FRESH_SESSION_THRESHOLD && !session_id.is_empty() {
        println!(
            "  {} Prompt too large (~{} tokens > {}k) — starting fresh session with handoff notes",
            "↻".cyan(), assembled.estimated_tokens, session::TOKEN_FRESH_SESSION_THRESHOLD / 1000
        );
        state.session_id = None;
        return Ok(()); // Re-enter Execute with cleared session_id
    }

    let result = if session_id.is_empty() {
        claude.run_execute_fresh(worktree, &assembled.text)
    } else {
        claude.run_execute(worktree, &assembled.text, &session_id)
    }?;

    let output = match result {
        ClaudeResult::BudgetExhausted => {
            println!("\n  {} BUDGET EXCEEDED — {} incomplete", "✗".red().bold(), story.id);
            println!("  No changes were committed.");
            println!("  Increase max_budget_usd in .pipeline/config.toml and retry.");
            state.increment_cycle();
            return Ok(());
        }
        ClaudeResult::SessionExpired => {
            println!(
                "\n  {} Session expired for {} — starting fresh session with handoff notes",
                "↻".yellow(), story.id
            );
            state.session_id = None;
            // Re-enter Execute; on next iteration the assembled prompt will include handoff preamble
            return Ok(());
        }
        ClaudeResult::Success(o) => o,
    };

    // Save session_id for subsequent stories
    if !output.session_id.is_empty() {
        state.session_id = Some(output.session_id.clone());
    }
    state.add_cost(output.total_cost_usd);
    state.increment_cycle();

    let so = &output.structured_output;
    let status = runner::get_str(so, "status").unwrap_or("unknown");
    let files_changed = runner::get_str_array(so, "files_changed");
    let test_passed = runner::get_bool(so, "test_passed").unwrap_or(false);
    let tests_run = runner::get_str_array(so, "tests_run");
    let progress_update = runner::get_str(so, "progress_update").unwrap_or("").to_string();
    let handoff_note = runner::get_str(so, "handoff_note").unwrap_or("").to_string();

    // Accumulate progress notes and truncate if needed
    if !progress_update.is_empty() {
        let _ = prompt::update_progress(repo_root, &issue_ctx.key, &story.id, &progress_update);
        let _ = session::truncate_progress_if_needed(repo_root);
    }

    // Save handoff_note for session continuity (US-016)
    if !handoff_note.is_empty() {
        let _ = session::save_handoff_note(repo_root, &issue_ctx.key, &story.id, &handoff_note);
    }

    // Capture commit hash from worktree HEAD
    let commit_hash = crate::git::head_commit(worktree).unwrap_or_else(|_| "unknown".to_string());

    // Store results in story state
    if let Some(s) = state.stories.get_mut(state.current_story_idx) {
        s.files_changed = files_changed.clone();
        s.test_passed = test_passed;
        s.commit_hash = Some(commit_hash);
    }

    println!("  {} Status: {} | Tests: {} ({} run) | Files: {}",
        if status == "done" { "✓".green() } else { "⚠".yellow() },
        status.bold(),
        if test_passed { "passed".green() } else { "FAILED".red() },
        tests_run.len(),
        files_changed.len()
    );

    if status == "blocked" {
        let blocker = runner::get_str(so, "blocker").unwrap_or("unknown blocker").to_string();
        println!("  {} Blocked: {}", "✗".red(), blocker);
        state.block_story(&blocker);
        // Still proceed to guard+gate so human can see and decide
    }

    state.phase = Phase::Guard;
    Ok(())
}

fn run_guards(
    state: &mut PipelineState,
    guards: &GuardRunner,
    worktree: &Path,
) -> Result<()> {
    println!("\n{}", "Guards".bold());

    let files_changed: Vec<String> = state
        .current_story()
        .map(|s| s.files_changed.clone())
        .unwrap_or_default();

    if files_changed.is_empty() {
        println!("  {} No files to check", "–".dimmed());
        state.phase = Phase::Gate2;
        return Ok(());
    }

    let violations = guards.run(&files_changed, worktree);
    let error_count = violations.iter().filter(|v| v.severity == crate::guards::Severity::Error).count();
    let warn_count = violations.iter().filter(|v| v.severity == crate::guards::Severity::Warn).count();

    if violations.is_empty() {
        println!("  {} All guards passed", "✓".green());
    } else {
        for v in &violations {
            let sev = match v.severity {
                crate::guards::Severity::Error => "ERROR".red().bold(),
                crate::guards::Severity::Warn => "WARN ".yellow(),
            };
            println!("  {} {} — {}:{} — {}", sev, v.rule.bold(), v.file, v.line, v.snippet.dimmed());
        }
        println!("  {} {} errors, {} warnings", "↑".dimmed(), error_count, warn_count);
    }

    // Store guard results in story state for gate2 display
    if let Some(story) = state.stories.get_mut(state.current_story_idx) {
        story.guard_errors = error_count as u32;
        story.guard_warns = warn_count as u32;
    }

    state.phase = Phase::Gate2;
    Ok(())
}

async fn gate2(
    state: &mut PipelineState,
    repo_root: &Path,
    cfg: &PipelineConfig,
    backlog: &BacklogClient,
    max_stories_per_session: u32,
) -> Result<()> {
    let story = state.current_story()
        .ok_or_else(|| anyhow::anyhow!("No current story for gate"))?
        .clone();

    // Enforce cycle limit before going to the gate
    if cycle_limit_reached(story.cycle_count, cfg.max_cycles_per_story) {
        println!(
            "\n  {} {} reached cycle limit ({}/{}) — blocking for human intervention",
            "LIMIT".red().bold(), story.id, story.cycle_count, cfg.max_cycles_per_story
        );
        state.block_story(&format!(
            "Cycle limit {}/{} reached without approval",
            story.cycle_count, cfg.max_cycles_per_story
        ));
        let msg = format!(
            "Story {} blocked: reached cycle limit {}/{}. Manually resolve then `pipeline resume {}`.",
            story.id, story.cycle_count, cfg.max_cycles_per_story, state.issue_key
        );
        let _ = backlog.add_comment(&state.issue_key, &msg).await;
        state.phase = Phase::Abandoned;
        return Ok(());
    }

    let guard_pass = guard_passed(story.guard_errors);
    let summary = GateSummary {
        issue_key: &state.issue_key,
        story_id: &story.id,
        story_title: &story.title,
        guard_pass,
        error_count: story.guard_errors as usize,
        warn_count: story.guard_warns as usize,
        test_passed: story.test_passed,
        commit_hash: story.commit_hash.as_deref().unwrap_or("unknown"),
        files_changed: story.files_changed.len(),
        cost_this_issue: state.total_cost_usd,
        branch: &state.branch,
        cycle: story.cycle_count,
        max_cycles: cfg.max_cycles_per_story,
    };

    // Cache agent_id on first gate to avoid re-fetching every story
    let gate = if let Some(ref id) = state.cached_agent_id.clone() {
        GateClient::with_agent_id(&cfg.openfang_url, id)
    } else {
        match GateClient::new(&cfg.openfang_url).await {
            Ok(g) => g,
            Err(e) => {
                eprintln!("  {} OpenFang unavailable ({}): using terminal gate", "WARN".yellow(), e);
                return terminal_gate_fallback(state, repo_root, cfg, backlog, max_stories_per_session).await;
            }
        }
    };

    let gate_result = gate.post_and_wait(&summary).await;

    match gate_result {
        Ok(GateDecision::Approved) => {
            on_story_approved(state, repo_root, cfg, backlog, max_stories_per_session).await?;
        }
        Ok(GateDecision::Rejected { notes }) => {
            let raw_notes = notes.unwrap_or_default();
            on_story_feedback(state, repo_root, &story, &raw_notes, backlog).await?;
        }
        Err(e) => {
            eprintln!("  {} Gate poll error: {} — using terminal fallback", "WARN".yellow(), e);
            terminal_gate_fallback(state, repo_root, cfg, backlog, max_stories_per_session).await?;
        }
    }

    Ok(())
}

async fn terminal_gate_fallback(
    state: &mut PipelineState,
    repo_root: &Path,
    cfg: &PipelineConfig,
    backlog: &BacklogClient,
    max_stories_per_session: u32,
) -> Result<()> {
    let story = state.current_story().ok_or_else(|| anyhow::anyhow!("No current story"))?.clone();

    println!("\n  {} OpenFang not available — terminal gate for {}", "FALLBACK".yellow(), story.id);
    println!("  [A] Approve   [R] Reject   [F] Flag   [P] Pause   [Q] Quit");
    print!("  Choice: ");
    io::Write::flush(&mut io::stdout()).ok();

    let stdin = io::stdin();
    let line = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;
    match line.trim().to_lowercase().as_str() {
        "a" | "approve" => {
            on_story_approved(state, repo_root, cfg, backlog, max_stories_per_session).await?;
        }
        "r" | "reject" => {
            println!("  Enter rejection notes (hard reject — commit will be reverted):");
            print!("  > ");
            io::Write::flush(&mut io::stdout()).ok();
            let notes = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;
            on_story_feedback(state, repo_root, &story, notes.trim(), backlog).await?;
        }
        "f" | "flag" => {
            println!("  Enter flag feedback (soft feedback — Claude will amend):");
            print!("  > ");
            io::Write::flush(&mut io::stdout()).ok();
            let fb = stdin.lock().lines().next().unwrap_or(Ok(String::new()))?;
            let flag_notes = format!("FLAG: {}", fb.trim());
            on_story_feedback(state, repo_root, &story, &flag_notes, backlog).await?;
        }
        "p" | "pause" => {
            // US-015: Pause — add Backlog comment and exit cleanly
            let msg = format!(
                "Pipeline paused on story {} by human gate. Run `pipeline resume {}` to continue.",
                story.id, state.issue_key
            );
            println!("  {} Pipeline paused. Run `pipeline resume {}` to continue.", "⏸".yellow(), state.issue_key);
            let _ = backlog.add_comment(&state.issue_key, &msg).await;
            std::process::exit(0);
        }
        _ => {
            println!("  {} Quitting", "⏸".yellow());
            std::process::exit(0);
        }
    }

    Ok(())
}

/// US-013/016: Story approved — advance story index, apply Ralph loop if needed.
async fn on_story_approved(
    state: &mut PipelineState,
    _repo_root: &Path,
    _cfg: &PipelineConfig,
    _backlog: &BacklogClient,
    max_stories_per_session: u32,
) -> Result<()> {
    println!("  {} Story approved", "✓".green());

    let more = state.advance_story();

    if !more {
        state.phase = Phase::Pr;
        return Ok(());
    }

    // Ralph loop: clear session every N stories so Claude doesn't drift (US-016)
    if is_ralph_threshold(state.current_story_idx, max_stories_per_session) {
        println!(
            "  {} Ralph loop: resetting session at story {} (every {} stories)",
            "↻".cyan(),
            state.current_story_idx,
            max_stories_per_session
        );
        state.session_id = None;
    }

    state.phase = Phase::Execute;
    Ok(())
}

/// US-013/014: Gate2 feedback — differentiate FLAG (soft) vs hard reject.
///
/// FLAG path (US-013): save feedback, increment flag_count, warn at ≥3,
///   set rejection_notes, phase → GapFix (Claude amends the commit).
///
/// REJECT path (US-014): save rejection reason, increment rejection_count,
///   if ≥3 → escalate (Backlog comment + Abandoned).
///   Otherwise: revert commit, clear session_id, phase → Execute (redo from scratch).
async fn on_story_feedback(
    state: &mut PipelineState,
    repo_root: &Path,
    story: &StoryState,
    notes: &str,
    backlog: &BacklogClient,
) -> Result<()> {
    if feedback::is_flag(notes) {
        // US-013: Soft flag — Claude amends without full revert
        let flag_text = feedback::extract_flag_text(notes);
        println!("  {} FLAG: {}", "⚑".yellow(), flag_text);

        let _ = feedback::save_flag_feedback(repo_root, &state.issue_key, &story.id, &flag_text);

        if let Some(s) = state.stories.get_mut(state.current_story_idx) {
            s.flag_count += 1;
            s.status = StoryStatus::Flagged;
            s.rejection_notes = Some(flag_text.clone());
        }

        let flag_count = state.stories[state.current_story_idx].flag_count;
        if flag_count >= 3 {
            println!(
                "  {} Story {} flagged {} times — consider a hard rejection",
                "WARN".yellow(), story.id, flag_count
            );
        }

        state.phase = Phase::GapFix;
    } else {
        // US-014: Hard reject — revert commit and redo from scratch
        let notes_trimmed = notes.trim();
        println!("  {} REJECT: {}", "✗".red().bold(), notes_trimmed);

        let cycle = story.cycle_count;
        let _ = feedback::save_rejection_reason(repo_root, &state.issue_key, &story.id, cycle, notes_trimmed);

        if let Some(s) = state.stories.get_mut(state.current_story_idx) {
            s.rejection_count += 1;
            s.status = StoryStatus::InProgress;
            s.rejection_notes = Some(notes_trimmed.to_string());
        }

        let rejection_count = state.stories[state.current_story_idx].rejection_count;

        if rejection_count >= 3 {
            // Escalate after 3 hard rejections
            let msg = format!(
                "Story {} rejected {} times without approval. Manual intervention required.\n\
                Resolve the issues and run `pipeline resume {}` to continue.",
                story.id, rejection_count, state.issue_key
            );
            println!("  {} {}", "ESCALATE".red().bold(), msg);
            let _ = backlog.add_comment(&state.issue_key, &msg).await;
            state.block_story(&format!("Hard-rejected {} times without approval", rejection_count));
            state.phase = Phase::Abandoned;
        } else {
            // Revert the story's last commit so Claude starts clean
            if let Err(e) = crate::git::revert_story_commit(&state.worktree_path) {
                eprintln!(
                    "  {} Failed to revert commit: {} — continuing without revert",
                    "WARN".yellow(), e
                );
            } else {
                println!("  {} Commit reverted — re-executing story from scratch", "↻".yellow());
            }

            // Clear session_id: force fresh session so Claude doesn't see the reverted work
            state.session_id = None;
            state.phase = Phase::Execute;
        }
    }

    Ok(())
}

fn gapfix(
    state: &mut PipelineState,
    repo_root: &Path,
    worktree: &Path,
    issue_ctx: &IssueContext,
    role_file: &Path,
    cfg: &PipelineConfig,
    claude: &ClaudeRunner,
) -> Result<()> {
    let story = state.current_story()
        .ok_or_else(|| anyhow::anyhow!("No current story for gapfix"))?
        .clone();

    println!("\n{} {}: {}", "GapFix".bold(), story.id.cyan(), story.title);

    let session_id = state.session_id.clone().unwrap_or_default();

    let feedback_text = story.rejection_notes.clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Address any test failures and code quality issues.".to_string());

    let story_ctx = StoryContext {
        id: story.id.clone(),
        title: story.title.clone(),
        completed_stories: state.completed_story_ids(),
        session_note: String::new(),
        feedback: Some(feedback_text),
    };

    let mut assembled = prompt::assemble(
        repo_root,
        issue_ctx,
        role_file,
        PromptPhase::GapFix,
        Some(&story_ctx),
        cfg.repo_map_lines,
    )?;

    // On fresh session (expired or post-reject): prepend handoff preamble so Claude re-orients
    if session_id.is_empty() {
        let completed_ids = state.completed_story_ids();
        if !completed_ids.is_empty() {
            let notes = session::load_handoff_notes(repo_root, &issue_ctx.key, &completed_ids);
            let preamble = session::build_handoff_preamble(&notes);
            if !preamble.is_empty() {
                println!("  {} Injecting {} handoff notes for fresh-session gapfix", "↻".cyan(), notes.len());
                assembled.text = format!("{}\n\n---\n\n{}", preamble, assembled.text);
            }
        }
    }

    let result = if session_id.is_empty() {
        claude.run_execute_fresh(worktree, &assembled.text)
    } else {
        claude.run_gapfix(worktree, &assembled.text, &session_id)
    }?;

    let output = match result {
        ClaudeResult::BudgetExhausted => {
            println!("  {} Budget exhausted during gapfix", "✗".red());
            state.phase = Phase::Guard;
            return Ok(());
        }
        ClaudeResult::SessionExpired => {
            // Clear session_id; re-enter GapFix next iteration with fresh session
            println!(
                "  {} Session expired during gapfix — retrying with fresh session",
                "↻".yellow()
            );
            state.session_id = None;
            return Ok(());
        }
        ClaudeResult::Success(o) => o,
    };

    if !output.session_id.is_empty() {
        state.session_id = Some(output.session_id.clone());
    }
    state.add_cost(output.total_cost_usd);
    state.increment_cycle();

    let files_changed = runner::get_str_array(&output.structured_output, "files_changed");
    if let Some(s) = state.stories.get_mut(state.current_story_idx) {
        s.files_changed = files_changed;
        s.status = StoryStatus::InProgress;
    }

    println!("  {} GapFix complete — re-running guards", "✓".green());
    state.phase = Phase::Guard;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn pr_phase(
    state: &mut PipelineState,
    repo_root: &Path,
    worktree: &Path,
    issue_ctx: &IssueContext,
    role_file: &Path,
    _cfg: &PipelineConfig,
    claude: &ClaudeRunner,
    backlog: &BacklogClient,
) -> Result<()> {
    println!("\n{}", "PR Phase".bold());

    let session_id = state.session_id.clone().unwrap_or_default();
    let completed: Vec<String> = state
        .stories
        .iter()
        .filter(|s| s.status == crate::state::StoryStatus::Done)
        .map(|s| s.id.clone())
        .collect();

    let story_ctx = StoryContext {
        id: "PR".into(),
        title: issue_ctx.summary.clone(),
        completed_stories: completed.clone(),
        session_note: String::new(),
        feedback: None,
    };

    let assembled = prompt::assemble(
        repo_root,
        issue_ctx,
        role_file,
        PromptPhase::Pr,
        Some(&story_ctx),
        0,
    )?;

    println!("  Calling Claude to open draft PR...");
    let result = if session_id.is_empty() {
        // No session to resume — start fresh (unusual but safe)
        claude.run_execute_fresh(worktree, &assembled.text)
    } else {
        claude.run_pr(worktree, &assembled.text, &session_id)
    }?;

    let output = match result {
        ClaudeResult::BudgetExhausted => {
            println!("  {} Budget exhausted during PR phase", "✗".red());
            return Ok(());
        }
        ClaudeResult::SessionExpired => {
            // PR phase lost its session — retry with a fresh session next iteration
            println!("  {} Session expired during PR phase — retrying fresh", "↻".yellow());
            state.session_id = None;
            return Ok(());
        }
        ClaudeResult::Success(o) => o,
    };

    state.add_cost(output.total_cost_usd);

    let pr_url = runner::get_str(&output.structured_output, "pr_url")
        .unwrap_or("(unknown)")
        .to_string();

    state.pr_url = Some(pr_url.clone());
    state.phase = Phase::Complete;

    println!("  {} PR created: {}", "✓".green().bold(), pr_url.cyan());
    println!("  {} Total cost: ${:.2}", "✓".green(), state.total_cost_usd);

    // Add Backlog comment with PR link
    let comment = format!(
        "Pipeline completed.\nPR: {}\nStories: {}\nTotal cost: ${:.2}",
        pr_url,
        completed.join(", "),
        state.total_cost_usd
    );
    let _ = backlog.add_comment(&issue_ctx.key, &comment).await;

    Ok(())
}

// ---------------------------------------------------------------------------
// Pure helpers — extracted for testability
// ---------------------------------------------------------------------------

/// Returns true if the Claude session should be reset before the next story.
/// Fires when `story_idx` is a non-zero multiple of `max_stories_per_session`.
/// Returns false if `max_stories_per_session` is 0 (disabled).
pub(crate) fn is_ralph_threshold(story_idx: usize, max_stories_per_session: u32) -> bool {
    if max_stories_per_session == 0 || story_idx == 0 {
        return false;
    }
    story_idx.is_multiple_of(max_stories_per_session as usize)
}

/// Returns true if the story has exhausted its cycle budget.
pub(crate) fn cycle_limit_reached(cycle_count: u32, max_cycles: u32) -> bool {
    max_cycles > 0 && cycle_count >= max_cycles
}

/// Returns true if the story passed all guards (no error-severity violations).
pub(crate) fn guard_passed(guard_errors: u32) -> bool {
    guard_errors == 0
}

// ---------------------------------------------------------------------------
// Stale state recovery
// ---------------------------------------------------------------------------

async fn warn_stale_states(repo_root: &Path, backlog: &BacklogClient, _cfg: &PipelineConfig) {
    let stale = PipelineState::find_stale(repo_root);
    for s in stale {
        eprintln!(
            "  {} Stale pipeline state for {} (last updated: {}) — run `pipeline resume {}` or check manually",
            "WARN".yellow(),
            s.issue_key,
            s.last_updated.format("%Y-%m-%d %H:%M UTC"),
            s.issue_key
        );
        let msg = format!(
            "Pipeline may have crashed on this issue (state stale since {}). \
            Run `pipeline resume {}` or `pipeline abandon {}` to resolve.",
            s.last_updated.format("%Y-%m-%d"),
            s.issue_key,
            s.issue_key
        );
        let _ = backlog.add_comment(&s.issue_key, &msg).await;
        let _ = backlog.update_status(&s.issue_key, STATUS_OPEN).await;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Phase, Role, StoryState, StoryStatus};
    use std::path::PathBuf;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn make_state(n_stories: usize) -> PipelineState {
        let mut s = PipelineState::new(
            "OFANG-001",
            "Test issue",
            Role::Backend,
            "pipeline/OFANG-001",
            PathBuf::from("/tmp/wt"),
        );
        let stories: Vec<StoryState> = (1..=n_stories)
            .map(|i| StoryState {
                id: format!("US-{:03}", i),
                title: format!("Story {}", i),
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
            })
            .collect();
        s.set_stories(stories);
        s
    }

    // -----------------------------------------------------------------------
    // is_ralph_threshold
    // -----------------------------------------------------------------------

    #[test]
    fn test_ralph_fires_at_exact_multiple() {
        assert!(is_ralph_threshold(5, 5));
        assert!(is_ralph_threshold(10, 5));
        assert!(is_ralph_threshold(3, 3));
    }

    #[test]
    fn test_ralph_does_not_fire_at_non_multiple() {
        assert!(!is_ralph_threshold(4, 5));
        assert!(!is_ralph_threshold(6, 5));
        assert!(!is_ralph_threshold(1, 5));
    }

    #[test]
    fn test_ralph_does_not_fire_at_zero_idx() {
        // idx=0 always returns false — prevents reset before any stories run
        assert!(!is_ralph_threshold(0, 5));
        assert!(!is_ralph_threshold(0, 1));
    }

    #[test]
    fn test_ralph_disabled_when_max_is_zero() {
        // max_stories_per_session = 0 means disabled
        assert!(!is_ralph_threshold(5, 0));
        assert!(!is_ralph_threshold(10, 0));
    }

    #[test]
    fn test_ralph_fires_after_nth_story_in_state_simulation() {
        // Simulate: 6-story run with max_stories_per_session = 5.
        // Ralph should fire after story 5 (idx becomes 5).
        let mut s = make_state(6);
        s.session_id = Some("original-session".to_string());

        let max = 5u32;
        let mut ralph_fired = false;

        for _ in 0..5 {
            let more = s.advance_story();
            if more && is_ralph_threshold(s.current_story_idx, max) {
                s.session_id = None;
                ralph_fired = true;
            }
        }

        assert!(ralph_fired, "Ralph should have fired at story 5");
        assert!(s.session_id.is_none(), "session_id must be cleared by Ralph loop");
        assert_eq!(s.current_story_idx, 5);
    }

    // -----------------------------------------------------------------------
    // cycle_limit_reached
    // -----------------------------------------------------------------------

    #[test]
    fn test_cycle_limit_reached_at_max() {
        assert!(cycle_limit_reached(3, 3));
        assert!(cycle_limit_reached(4, 3)); // above max also blocked
    }

    #[test]
    fn test_cycle_limit_not_reached_below_max() {
        assert!(!cycle_limit_reached(2, 3));
        assert!(!cycle_limit_reached(0, 3));
    }

    #[test]
    fn test_cycle_limit_disabled_when_max_is_zero() {
        assert!(!cycle_limit_reached(99, 0));
    }

    // -----------------------------------------------------------------------
    // guard_passed
    // -----------------------------------------------------------------------

    #[test]
    fn test_guard_passed_with_zero_errors() {
        assert!(guard_passed(0));
    }

    #[test]
    fn test_guard_failed_with_errors() {
        assert!(!guard_passed(1));
        assert!(!guard_passed(5));
    }

    // -----------------------------------------------------------------------
    // State machine — approve path simulation
    // -----------------------------------------------------------------------

    #[test]
    fn test_approve_advances_to_execute_when_more_stories() {
        let mut s = make_state(3);
        // Simulate gate2 approve: advance story, check phase
        let more = s.advance_story();
        s.phase = if more { Phase::Execute } else { Phase::Pr };
        assert_eq!(s.phase, Phase::Execute);
        assert_eq!(s.current_story_idx, 1);
    }

    #[test]
    fn test_approve_advances_to_pr_when_last_story() {
        let mut s = make_state(1);
        let more = s.advance_story();
        s.phase = if more { Phase::Execute } else { Phase::Pr };
        assert_eq!(s.phase, Phase::Pr);
    }

    // -----------------------------------------------------------------------
    // State machine — reject path simulation
    // -----------------------------------------------------------------------

    #[test]
    fn test_reject_stores_notes_and_transitions_to_gapfix() {
        let mut s = make_state(2);
        let notes = Some("The handler is too long".to_string());
        // Simulate gate2 reject
        if let Some(story) = s.stories.get_mut(s.current_story_idx) {
            story.status = StoryStatus::Flagged;
            story.rejection_notes = notes.clone();
        }
        s.phase = Phase::GapFix;

        assert_eq!(s.phase, Phase::GapFix);
        assert_eq!(s.stories[0].status, StoryStatus::Flagged);
        assert_eq!(s.stories[0].rejection_notes, notes);
    }

    #[test]
    fn test_reject_notes_available_for_gapfix_prompt() {
        let mut s = make_state(2);
        s.stories[0].rejection_notes = Some("Missing error handling in POST /users".to_string());

        // GapFix reads rejection_notes to build prompt feedback
        let feedback = s.stories[s.current_story_idx]
            .rejection_notes
            .clone()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Address issues.".to_string());

        assert_eq!(feedback, "Missing error handling in POST /users");
    }

    // -----------------------------------------------------------------------
    // Guard results propagation
    // -----------------------------------------------------------------------

    #[test]
    fn test_guard_errors_stored_in_story_state() {
        let mut s = make_state(2);
        // Simulate run_guards storing results
        if let Some(story) = s.stories.get_mut(s.current_story_idx) {
            story.guard_errors = 2;
            story.guard_warns = 3;
        }
        let story = s.current_story().unwrap();
        assert!(!guard_passed(story.guard_errors));
        assert_eq!(story.guard_warns, 3);
    }

    #[test]
    fn test_guard_pass_propagates_to_gate_summary_fields() {
        let mut s = make_state(2);
        s.stories[0].guard_errors = 0;
        s.stories[0].guard_warns = 2;
        s.stories[0].test_passed = true;

        let story = s.current_story().unwrap();
        assert!(guard_passed(story.guard_errors));
        assert_eq!(story.guard_warns, 2);
        assert!(story.test_passed);
    }
}
