# Phase 1 — Pipeline Skeleton: Completion Report

**Completed:** 2026-04-14  
**Crate:** `crates/openfang-pipeline`  
**Binary:** `pipeline`  
**Commits:** `bcb4f76` (initial), `d9f6027` (review fixes)  

---

## Scope

Phase 1 delivers the `pipeline` CLI binary with four user stories fully implemented and tested:

| US | Story | File | Tests |
|----|-------|------|-------|
| US-000 | Startup auth checks | `commands/doctor.rs` | 6 |
| US-003 | Branch + worktree management | `git.rs` | 5 |
| US-005 | Prompt assembly engine | `prompt.rs` | 7 |
| US-018 | `pipeline setup` command | `commands/setup.rs` | 9 |
| — | Configuration + defaults | `config.rs` | 8 |
| — | State schema | `state.rs` | — |

**Total: 35 tests, 0 failures, 0 clippy warnings.**

---

## What was built

### US-000 — `pipeline doctor`
Runs three external auth checks before any pipeline work begins:

1. **Claude CLI** — probes with `claude -p --max-budget-usd 0.001 "Reply with the single word: ready"`. Detects auth failures by keyword matching on combined stdout+stderr. Strips `ANTHROPIC_API_KEY` from env to prefer stored token. Reports which auth mode is active (`setup-token` or `ANTHROPIC_API_KEY`).
2. **Backlog API** — hits `{backlog_base}/api/v2/projects?apiKey={key}&count=1` via curl with 10s connect / 15s max timeout.
3. **GitHub CLI** — runs `gh auth status`.

Output shows fix instructions for each failing check. `run_and_exit_on_failure()` is called at the start of every `pipeline run`.

Backlog credentials are optional at `pipeline doctor` time (shows a warning, does not block). Required at `pipeline run` time.

### US-003 — Branch + worktree management
Full git workspace lifecycle for each issue:

- `ensure_agent_dev(repo, base_branch)` — creates `agent/dev` from `base_branch` if absent, otherwise fast-forward merges (falls back to merge commit with clear error on conflict).
- `create_issue_branch(repo, issue_key)` — creates `pipeline/{issueKey}` from `agent/dev`. Resumes (checks out) if the branch already exists.
- `ensure_worktree(repo, issue_key, branch)` — creates `~/.pipeline/worktrees/{issueKey}/` via `git worktree add`. Reuses if already exists.
- `setup_issue_workspace()` — orchestrates all three, guards on uncommitted changes first.
- `cleanup_issue()` — removes worktree, checks out `base_branch`, force-deletes the issue branch (post-PR cleanup).

### US-005 — Prompt assembly engine
Assembles the full Claude prompt in the correct order:

```
1. CLAUDE.md (repo conventions)
2. PIPELINE/PROGRESS.md (if exists — mandatory after first story)
3. Role standards file (.pipeline/backend.md or frontend.md)
4. Backlog issue block (key, summary, type, priority, description)
5. Phase instruction block (Decompose / Execute / GapFix / PR)
6. Repo map (optional — `rg` over crates/, capped at N lines)
```

Writes audit file to `PIPELINE/PROMPT-{key}-{phase}.md` on every call.

Phase blocks are exact PRD spec — Claude receives structured JSON output requirements for each phase.

`update_progress(repo, issue_key, story_id, update)` appends to `PIPELINE/PROGRESS.md`. This accumulates codebase-pattern notes across stories (the "Ralph Wiggum Loop" context carrier).

### US-018 — `pipeline setup`
Bootstraps a repo for pipeline use:

1. Hard-stops if no `CLAUDE.md` (use `--force` to skip).
2. Creates `.pipeline/config.toml` (idempotent, full template with comments).
3. Creates `.pipeline/guards.toml` (8 guard rules, idempotent).
4. Generates `.pipeline/backend.md` and `.pipeline/frontend.md` by calling Claude with repo context. Falls back to placeholder templates if Claude is unavailable.
5. Creates `PIPELINE/` directory for runtime state.
6. Adds `PIPELINE/` to `.gitignore` (exact-line check, not substring match).

### Supporting modules

**`config.rs`** — `PipelineConfig` serde struct with all defaults. Config file at `.pipeline/config.toml`. Template functions return full TOML/TOML content as strings (using `concat!()` and builder functions to avoid raw-string delimiter collisions with TOML content).

**`state.rs`** — `PipelineState` struct serialised to `PIPELINE/STATE-{issueKey}.json`. Tracks phase, stories, cost, branch, worktree path, session ID. Methods: `new`, `save`, `load`, `exists`, `current_story`, `is_stale`. Phase 2 will write/read this on every loop iteration.

**`main.rs`** — Clap CLI wiring. `tracing_subscriber` initialised with `RUST_LOG` env filter (default `warn`). `pipeline run` calls doctor checks + git workspace setup. `pipeline resume` validates state exists. `pipeline status` reads all `STATE-*.json` files and prints a summary table.

---

## Bugs found and fixed during review

### Bug 1 — `find` OR groups not bounded by `-maxdepth 3` (`setup.rs`)

**Before:**
```rust
.args([".", "-maxdepth", "3", "-type", "f",
       "-name", "*.rs", "-o", "-type", "f", "-name", "*.ts", ...])
```
`find`'s operator precedence: `-a` binds tighter than `-o`. Only the first clause was constrained to depth 3 and type `f`. The `.ts`, `.tsx`, and `Cargo.toml` groups would traverse the entire tree.

**After:**
```rust
.args([".", "-maxdepth", "3", "-type", "f",
       "(", "-name", "*.rs", "-o", "-name", "*.ts",
       "-o", "-name", "*.tsx", "-o", "-name", "Cargo.toml", ")"])
```
Grouping brackets force `-maxdepth 3` and `-type f` to apply to all name alternatives.

---

### Bug 2 — `ensure_gitignore` matched substrings, not lines (`setup.rs`)

**Before:**
```rust
if content.contains(entry) { return Ok(()); }
```
A `.gitignore` containing `# see PIPELINE/ for runtime state` would prevent the actual `PIPELINE/` entry from ever being added.

**After:**
```rust
if content.lines().any(|l| l.trim() == entry) { return Ok(()); }
```
Only an exact `PIPELINE/` line (after trimming whitespace) suppresses re-insertion. Tested with a dedicated `test_ensure_gitignore_not_fooled_by_comment` test case.

---

## Test coverage summary

| Module | Tests | What is covered |
|--------|-------|-----------------|
| `doctor.rs` | 6 | `is_auth_error` keyword detection (8 positive + 5 negative), `DoctorResult::all_ok()` all combinations |
| `setup.rs` | 9 | `create_if_absent` (create/skip/refresh), `ensure_gitignore` (create/no-dup/append/comment edge case), `check_claude_md` (missing/force/present) |
| `config.rs` | 8 | defaults, TOML template validity, template key presence, guard rule structure, load from file, load error, file path helpers |
| `prompt.rs` | 7 | decompose assembly, execute assembly, progress.md injection, ordering invariants, missing CLAUDE.md error, update_progress |
| `git.rs` | 5 | branch_exists false, ensure_agent_dev creates, create_issue_branch, uncommitted changes clean/dirty |
| **Total** | **35** | |

---

## Known limitations (Phase 2 work)

- `pipeline run` currently only runs doctor + git setup. Full decompose → execute → gate → PR loop is Phase 2.
- `pipeline resume` shows a stub message. Full state-machine resumption is Phase 2.
- `pipeline logs` reads `~/.pipeline/logs/*.log` but no log writing infrastructure exists yet (Phase 2 adds structured log output).
- `run_claude_setup()` in setup.rs passes the prompt as a CLI positional argument. For repos with very large CLAUDE.md files (>200KB), this could approach system ARG_MAX. Practical repos are well under this limit. A stdin-pipe approach can be added if needed.
- `uuid` and `reqwest` deps are declared but not yet used. They are required for Phase 2 (session IDs and Backlog/OpenFang HTTP clients respectively).

---

## Phase 2 plan

| US | Story |
|----|-------|
| US-001 | Fetch Backlog issue via REST API |
| US-002 | Classify issue as Backend or Frontend |
| US-006 | Decompose phase — call Claude, parse plan JSON, write state |
| US-007 | Gate 1 — post OpenFang approval request, poll for decision |
| US-008 | Execute loop — story-by-story Claude calls, Ralph loop, cost tracking |
| US-009 | Test runner — run story test scope, report failures |
| US-010/011 | Guard runner — apply guards.toml rules after each commit |
| US-012 | Approval gate — async poll, re-post on expiry, handle approve/reject |
