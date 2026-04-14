# PRD: Autonomous Development Pipeline
**Version:** 0.10 — POC-3 Backlog API + OpenFang modules wired  
**Status:** Ready for Development  
**Issue tracker:** Backlog (Nulab)  
**Primary executor:** Claude CLI  
**Orchestrator:** OpenFang  
**Research document:** [`docs/dev-pipeline-research.md`](dev-pipeline-research.md) — tool comparisons, OpenFang capability audit, all references  

---

## Problem

Development tasks sit in the Backlog and are manually picked up, manually run through Claude CLI, and manually reviewed. This means:

- No consistent workflow from issue to PR — every run is ad-hoc
- No systematic quality checks before human review (hardcoding, arch violations slip through)
- Large tickets are implemented as one big chunk — hard to review, hard to test
- No oversight gates — Claude can go off-track with no catch point
- Token usage is wasteful — full workspace tests, long unstructured sessions
- No pause/resume — if a session crashes or context fills, work is lost
- Backlog issue status is updated manually after the fact

---

## Goal

Build an autonomous development pipeline where:

- A Backlog issue goes in, a reviewed, tested, committed PR comes out
- A human stays in control at every story boundary
- Bad patterns are caught automatically before the human sees the diff
- Large issues are broken into reviewable user stories
- **OpenFang's only job is to assemble the correct prompt** — Claude CLI does every phase of work
- Claude CLI uses `CLAUDE.md` + role standards as its primary repo knowledge

---

## Non-Goals

- Does not replace Claude CLI — Claude is the executor for every phase
- Does not auto-merge PRs — human approval always required before merge
- Does not manage infrastructure, deployments, or CI/CD
- Does not run multiple issues in parallel in v1
- Does not build its own LLM or reasoning layer — OpenFang never calls an LLM directly

---

## Research-Derived Design Updates

> Full research in `docs/dev-pipeline-research.md`. Summary of key design decisions from research:

| Finding | Source | Change to design |
|---------|--------|-----------------|
| OpenFang already has a Workflow Engine | OpenFang codebase | Pipeline phases wired as workflow steps — no custom state machine |
| OpenFang already has a Task Queue | OpenFang codebase | Stories posted to OpenFang task queue for execution tracking; `PIPELINE/STATE-{key}.json` is retained as the crash-safe file the pipeline binary owns directly |
| OpenFang already has Approval system | OpenFang codebase | Human gates use existing approval UI (dashboard badge) — no custom TUI |
| OpenFang webhooks can receive Backlog events | OpenFang codebase | Backlog webhook → `/hooks/agent` replaces polling |
| Repository map (Aider) reduces hallucination | Aider | 1–2k token repo map prepended to every prompt in US-005 |
| Two-tier testing: fast path + validation gate | SWE-agent | US-009 updated |
| `progress.md` accumulates codebase patterns | Chief | `handoff_note` upgraded to structured `progress.md` |
| `gh pr create --draft` immediately, convert on approval | Sweep.dev | US-017 updated |
| Add iteration cap alongside budget cap | Chief | US-008 updated |
| Structured failure diagnosis | SWE-agent | `failure_type` field added to execute schema |
| **Sessions are directory-scoped** | **POC-1** | **Git worktrees required (not optional) — pipeline always cd's to worktree before Claude call** |
| **`structured_output` field** | **POC-2** | **Schema-enforced JSON is in `response["structured_output"]`, not `response["result"]`** |
| **Guard patterns broken on real code** | **POC-4** | **5 of 8 rules revised; `no_unwrap_production` stays WARN; `business_logic_in_routes` redesigned** |
| **Repo map: marginal benefit** | **POC-5** | **Opt-in (default off), cap 100 lines. Claude navigates accurately without it.** |
| **progress.md reduces cost 38%** | **POC-11** | **Mandatory in every prompt — 145 tokens/story, significant accuracy + cost benefit** |
| **Decompose costs ~$0.24** | **POC-9** | **Default `max_budget_usd` raised to $1.00/story; decompose baseline documented** |

---

## How OpenFang Works (Core Principle)

OpenFang is a **prompt assembler and state machine**. Nothing more.

```
Backlog Issue JSON
  + CLAUDE.md (from the target repo)
  + PIPELINE/PROGRESS.md (accumulated codebase patterns — mandatory)
  + .pipeline/backend.md OR .pipeline/frontend.md
  + current pipeline phase instruction
          ↓
    OpenFang assembles → one clean prompt
          ↓
    claude -p --dangerously-skip-permissions \
           --output-format json \
           --max-budget-usd {per_story_limit} \
           --resume {session_id}   ← same session across stories
          ↓
    OpenFang reads response["structured_output"] → next state
          ↓
    Backlog API updated
```

Claude reads the codebase itself. OpenFang never pre-loads file lists, API contracts, or architecture documents — those are Claude's job to discover. The `CLAUDE.md` already captures codebase conventions; passing that is sufficient.

### OpenFang Existing Modules Used by the Pipeline

The pipeline is built **on top of** OpenFang's existing capabilities — not alongside them. Each module is used directly:

| OpenFang Module | Location | How pipeline uses it |
|-----------------|----------|---------------------|
| **Workflow Engine** | `openfang-kernel/src/workflow.rs` | Pipeline phases (decompose → execute → guard → gate) wired as `Sequential` workflow steps; gap-fix loop as `Loop{max_iterations: max_cycles}` |
| **Task Queue** | `openfang-memory/src/substrate.rs` | Each story posted via `task_post()`; claimed via `task_claim()`; completed via `task_complete()`. Resume = re-claim in-progress task. |
| **Approval System** | `openfang-kernel/src/approvals.rs` | `request_approval(agent_id, "story_gate", summary)` blocks pipeline at every story boundary. Dashboard shows pending badge. Human responds in Tauri app or terminal. |
| **Event Bus** | `openfang-kernel/src/event_bus.rs` | Each phase publishes completion event (e.g. `story_complete`, `gate_approved`). Next phase triggered automatically via `TriggerPattern::ContentMatch`. |
| **Metering** | `openfang-kernel/src/metering.rs` | `check_quota()` before each Claude call; `get_summary()` for cost display at gate; per-issue cumulative spend. |
| **Webhook receiver** | `openfang-types/src/webhook.rs` | `POST /hooks/agent` receives Backlog webhook events (v2 upgrade from polling). |
| **Process Manager** | `openfang-runtime/src/process_manager.rs` | Claude CLI managed as persistent process — `--resume` sessions stay warm. |
| **Dashboard (Tauri)** | `openfang-api/static/index_body.html` | Pipeline tab (US-019) shows live story progress, guard results, cost, PR status. Gate approval via inline buttons. |
| **A2A Protocol** | `openfang-runtime/src/a2a.rs` | Future: external tools (Slack bot, GitHub Actions) submit issues via A2A REST. Out of scope v1. |

> See `docs/dev-pipeline-research.md` Section 1 for full capability audit of all 14 OpenFang crates.

---

## Architecture

```
Backlog Issue
     ↓
OpenFang Pipeline (state machine)
  ├─ Validate Claude CLI auth (startup check)
  ├─ Fetch issue + detect dependencies (Backlog API)
  ├─ Classify role (deterministic from issue labels)
  ├─ Create agent/dev branch + git worktree at ~/.pipeline/worktrees/{issueKey}/
  ├─ Assemble prompt (CLAUDE.md + PROGRESS.md + role standards + phase instruction + issue)
  ├─ cd {worktree_path} && call claude -p → capture session_id (CRITICAL: session is directory-scoped)
  ├─ Run guards on files_changed (grep — no LLM)
  ├─ Show approval gate → human decision
  ├─ On next story: cd {worktree_path} && claude --resume {session_id}
  ├─ structured_output field holds schema-enforced JSON; result field holds text summary
  ├─ On all stories approved: PR pipeline/{issueKey} → agent/dev
  └─ Update Backlog (status + PR comment)
```

> **POC findings baked in:** Sessions are directory-scoped (POC-1) — worktrees required. Structured output in `response["structured_output"]` (POC-2). progress.md reduces cost 38% (POC-11).

---

## OpenFang Fork Setup

The pipeline work lives in a fork of `RightNow-AI/openfang` so that:
- Pipeline-specific changes (new crate, new dashboard tab, new API routes) stay isolated from upstream
- Upstream bug fixes and features can be cherry-picked cleanly via a tracked remote
- The pipeline has its own release cycle independent of the upstream OpenFang release

**One-time fork setup:**
```bash
# 1. Fork on GitHub: RightNow-AI/openfang → your-org/openfang
#    (GitHub UI → "Fork" button)

# 2. Clone your fork
git clone https://github.com/your-org/openfang
cd openfang

# 3. Track upstream for future patches
git remote add upstream https://github.com/RightNow-AI/openfang
git fetch upstream

# 4. Work on your feature branch
git checkout -b feature/dev-pipeline
```

**Pulling upstream changes (when needed):**
```bash
git fetch upstream
git merge upstream/main --no-ff -m "chore: sync upstream main"
# Resolve any conflicts in modified files, then push
git push origin feature/dev-pipeline
```

**What the fork adds (does not exist upstream):**
| Addition | Location | Purpose |
|----------|----------|---------|
| `crates/openfang-pipeline/` | New crate | Orchestrator — prompt assembly, state machine, Claude CLI runner |
| Pipeline tab | `crates/openfang-api/static/index_body.html` | Dashboard visibility (US-019) |
| `GET /api/pipeline/status` | `crates/openfang-api/src/routes.rs` | Tab data endpoint |
| `POST /api/pipeline/gate/{storyId}/{decision}` | same | Inline gate approval (US-019) |

The Tauri desktop app (`crates/openfang-desktop/`) requires **no changes** — it is a webview shell and picks up the new tab automatically.

---

## Branch Model

```
{base_branch}  (configurable: "dev" default, "main" for repos without a dev branch)
  │
  └─ agent/dev  (long-lived pipeline integration branch — synced from {base_branch})
        │
        ├─ pipeline/OFANG-101  →  PR → agent/dev  ← reviewed here
        │       ↓ merged
        ├─ pipeline/OFANG-102  (branched from agent/dev — already has OFANG-101)
        │       ↓ merged
        ├─ pipeline/OFANG-103
        │       ↓ merged
        └─ (human decision) agent/dev → {base_branch}  (release PR — deliberate, not automatic)
```

**`base_branch` is the working branch for that repo** — set in `.pipeline/config.toml`:
```toml
base_branch = "dev"    # most repos: dev is the working branch
# base_branch = "main" # for repos that work directly off main
```

`agent/dev` is always named `agent/dev` regardless of `base_branch`. It syncs from `{base_branch}` before each new issue. Promotion from `agent/dev` → `{base_branch}` is a human decision — the pipeline never touches `{base_branch}` directly.

**Why this model:**
- Chained issues never block each other — `agent/dev` accumulates approved work; each new branch starts from there
- PRs are small and per-issue — reviewed against `agent/dev`, not cluttering `{base_branch}` PR queue
- `{base_branch}` stays clean — only deliberately shipped batches reach it
- Works for `dev`-based repos (majority) and `main`-based repos equally

**Dependency rule:** if OFANG-102 has OFANG-101 as a related/parent issue in Backlog, pipeline waits for OFANG-101's merge into `agent/dev` before branching OFANG-102. Checked via `git log agent/dev --grep="{OFANG-101}"` before creating the new branch.

---

## Repo Configuration

Each repo that uses the pipeline checks in a `.pipeline/` directory alongside the existing `CLAUDE.md` and `docs/BACKLOG.md`:

```
CLAUDE.md                    ← primary repo knowledge (already exists)
docs/
  BACKLOG.md                 ← Backlog project reference: URLs, user IDs, comment style rules,
                                active issue context. Claude reads this; pipeline reads config from it.
.pipeline/
  config.toml                ← Backlog project, role labels, session limits, cost limits
  backend.md                 ← backend quality gates, test patterns, conventions
  frontend.md                ← frontend quality gates, live verification steps
  guards.toml                ← pattern-match rules for auto-detecting violations
```

**`docs/BACKLOG.md` convention:** Every repo using the pipeline maintains a `docs/BACKLOG.md` that documents:
- Project key, base URL, project ID
- Numeric user IDs for `notifiedUserId[]` (required for Backlog comment notifications — plain userIds don't trigger notifications)
- Comment style rules for that project (e.g. plain text only, no markdown)
- Active issue status summary (Claude keeps this current)

The pipeline binary reads `docs/BACKLOG.md` at startup to extract `backlog_base` and `project_key` as an alternative to `config.toml`. Claude references it when writing Backlog comments to follow the project's style rules.

No session template directory. `CLAUDE.md` is the session template — it already describes the codebase. `.pipeline/backend.md` and `.pipeline/frontend.md` add role-specific quality gates on top.

**Both role files must include this rule — it is non-negotiable:**
```
PUSH CONTRACT: You must not push to remote during IMPLEMENT or FIX phases.
Commit locally only. The pipeline will push in the OPEN PR phase.
Pushing early causes force-push conflicts during flag/amend cycles.
```

---

## Prompt Structure (What OpenFang Assembles)

Every Claude CLI call receives this exact structure — nothing more, nothing less:

```
{CLAUDE.md full contents}

---

{.pipeline/backend.md OR .pipeline/frontend.md}

---

BACKLOG ISSUE: {issueKey}
Summary: {summary}
Description: {description}
Category: {category}
Priority: {priority}

---

CURRENT PHASE: {phase-specific instruction block}
```

The phase instruction block is the only thing that changes between calls. Everything above it is constant for the duration of the issue.

---

## Claude CLI Invocation

Every Claude CLI call uses `--output-format json` **and** `--json-schema`. Output extraction is deterministic — never instruction-dependent. "Output only this JSON" in the prompt is a hint for Claude's reasoning; `--json-schema` is the enforcement mechanism. These are never separated.

**Decompose phase (first call, new session):**
```bash
claude -p \
  --dangerously-skip-permissions \
  --output-format json \
  --max-budget-usd 0.50 \
  --json-schema '{
    "type":"object","required":["stories","session_note"],
    "properties":{
      "stories":{"type":"array","items":{
        "type":"object","required":["id","title","depends_on"],
        "properties":{
          "id":{"type":"string"},
          "title":{"type":"string"},
          "depends_on":{"type":"array","items":{"type":"string"}}
        }
      }},
      "session_note":{"type":"string"}
    }
  }' \
  < prompt.md
```
Response JSON includes `session_id`. OpenFang saves this to `PIPELINE/STATE-{key}.json`.

**Execute / fix phases (continuing session):**
```bash
claude -p \
  --dangerously-skip-permissions \
  --output-format json \
  --max-budget-usd 1.00 \
  --resume {session_id} \
  --json-schema '{
    "type":"object",
    "required":["story_id","status","files_changed","tests_run","test_passed","handoff_note","failure_type","progress_update"],
    "properties":{
      "story_id":{"type":"string"},
      "status":{"type":"string","enum":["done","blocked"]},
      "blocker":{"type":["string","null"]},
      "files_changed":{"type":"array","items":{"type":"string"}},
      "tests_run":{"type":"array","items":{"type":"string"}},
      "test_passed":{"type":"boolean"},
      "handoff_note":{"type":"string"},
      "failure_type":{"type":"string","enum":["none","test_failure","compilation_error","timeout","budget_exhausted","unknown"]},
      "progress_update":{"type":"string"}
    }
  }' \
  < prompt.md
```

**`handoff_note` + `progress_update` (Chief-inspired `progress.md` pattern):**  
Every execute-phase response includes:
- `handoff_note` — one paragraph: what was done, codebase state, what the next story needs to know. Crash-safe: if `--resume` fails, this primes a new session.
- `progress_update` — structured patterns discovered this story: codebase gotchas, file dependencies, test quirks.

OpenFang accumulates `progress_update` entries into `PIPELINE/progress.md`. This file is prepended to every subsequent prompt — Claude builds institutional knowledge without re-discovering the same patterns across issues. ~200 tokens per story added; value compounds over time.

**`failure_type` field (SWE-agent-inspired):**  
Execute phase also outputs `failure_type: "none | test_failure | compilation_error | timeout | budget_exhausted | unknown"`. OpenFang uses this to decide: auto-retry on `timeout`, escalate to human on `compilation_error`, recommend story split on `budget_exhausted` twice.

**PR phase:**
```bash
claude -p \
  --dangerously-skip-permissions \
  --output-format json \
  --max-budget-usd 0.50 \
  --resume {session_id} \
  --json-schema '{
    "type":"object","required":["pr_url","commits"],
    "properties":{
      "pr_url":{"type":"string"},
      "commits":{"type":"array","items":{"type":"string"}}
    }
  }' \
  < prompt.md
```

**New session after handoff (expired session or over story threshold):**
```bash
claude -p \
  --dangerously-skip-permissions \
  --output-format json \
  --max-budget-usd 1.00 \
  --json-schema '{... same as execute schema ...}' \
  < fresh_prompt_with_handoff_note.md
```
No `--resume`. New `session_id` saved to state.

---

## Proof of Concepts (Prerequisites to Full Development)

> Each POC is a small, self-contained experiment with a clear pass/fail criterion.  
> All POCs must pass before the corresponding user stories enter implementation.  
> Time-boxed: each POC should take no more than half a day.

---

### POC-1: Claude CLI Session Resume Across Gate Pauses

**Validates:** US-008, US-016 — the entire multi-story session model  
**Risk if skipped:** Session resume is the load-bearing assumption. If `--resume` fails or expires quickly, the whole continuity model breaks.

**Experiment:**
```bash
# Step 0: Confirm session_id field exists in --output-format json response
# (must do this first — the field name is assumed, never verified)
claude -p --output-format json --dangerously-skip-permissions \
  "Say the word hello." \
  | jq 'keys'
# Expected output includes "session_id" — if not, document actual field name

# Step 1: Start a real session with codebase work
cd /Users/rajesh/Documents/GitHub/openfang
SESSION_RESPONSE=$(claude -p --output-format json --dangerously-skip-permissions \
  "Read CLAUDE.md and tell me what the default API port is.")
echo "$SESSION_RESPONSE" | jq .
SESSION_ID=$(echo "$SESSION_RESPONSE" | jq -r '.session_id')
echo "Session ID: $SESSION_ID"

# Step 2: Simulate a gate pause (wait 30 min), then resume from SAME directory
claude -p --output-format json --dangerously-skip-permissions \
  --resume "$SESSION_ID" \
  "What was the API port you found in the previous message?"
# Pass: Claude recalls the port number without re-reading the file

# Step 3: Resume from a DIFFERENT directory (simulates pipeline running from different path)
cd /tmp
claude -p --output-format json --dangerously-skip-permissions \
  --resume "$SESSION_ID" \
  "What file did you read in the first message?"
# Critical: does cross-directory resume work? Document result.

# Step 4: Simulate story completion — make a commit, then resume
cd /Users/rajesh/Documents/GitHub/openfang
git checkout -b poc/session-test
echo "# poc test" > /tmp/poc_test.md
# (Claude makes a commit via the session, then we resume for the next story)
claude -p --output-format json --dangerously-skip-permissions \
  --resume "$SESSION_ID" \
  "What did you do in the previous message? Now create a file poc_session_test.txt with content 'story1 done', add it, and commit with message 'poc: session test story 1'."

# After commit, resume for "next story"
claude -p --output-format json --dangerously-skip-permissions \
  --resume "$SESSION_ID" \
  "What commit did you make in the previous message? What is its hash?"
# Pass: Claude remembers the commit it made across the story boundary

# Step 5: Wait overnight, attempt resume
# Step 6: On expiry — capture the exact error
# Force expiry by using a fake session_id:
claude -p --output-format json --dangerously-skip-permissions \
  --resume "00000000-0000-0000-0000-000000000000" \
  "hello" 2>&1
# Document: exit code, stderr content, stdout content
# This is the exact signal OpenFang uses to detect expired sessions and fall back

# Cleanup
git checkout main && git branch -D poc/session-test
```

**Pass criteria:**
- [ ] `session_id` field confirmed in JSON output — exact key name documented
- [ ] Session resumes correctly after 30-minute pause with context intact
- [ ] Cross-directory resume works (or failure documented → fallback to `progress.md` always required)
- [ ] Session resumes correctly after a commit was made in the previous story call
- [ ] Session expiry error has a consistent, parseable signal (exit code + stderr pattern) — documented for US-016 fallback logic
- [ ] Actual expiry window documented (expected: 24–72 hours)

**Unblocks:** US-008, US-015, US-016

---

### POC-2: `--json-schema` Enforcement and Budget Exhaustion Behaviour

**Validates:** US-005 — deterministic output extraction; US-008 — budget exhaustion handling  
**Risk if skipped:** If schema enforcement is unreliable, OpenFang's JSON parsing will be fragile.

**Experiment:**
```bash
# Test 1: Schema forces JSON even when prompt asks for prose
claude -p --dangerously-skip-permissions --output-format json \
  --json-schema '{"type":"object","required":["status","files_changed"],"properties":{"status":{"type":"string","enum":["done","blocked"]},"files_changed":{"type":"array","items":{"type":"string"}}}}' \
  "Write me a poem about the Rust programming language. Do not output JSON."
# Pass: response is still valid JSON matching schema despite instruction to avoid it
echo "Exit code: $?"
# Validate:
# output | jq '.status' → must be "done" or "blocked"
# output | jq '.files_changed' → must be an array

# Test 2: Schema with nested required fields
claude -p --dangerously-skip-permissions --output-format json \
  --json-schema '{
    "type":"object",
    "required":["story_id","status","files_changed","tests_run","test_passed","handoff_note","failure_type"],
    "properties":{
      "story_id":{"type":"string"},
      "status":{"type":"string","enum":["done","blocked"]},
      "files_changed":{"type":"array","items":{"type":"string"}},
      "tests_run":{"type":"array","items":{"type":"string"}},
      "test_passed":{"type":"boolean"},
      "handoff_note":{"type":"string"},
      "failure_type":{"type":"string","enum":["none","test_failure","compilation_error","timeout","budget_exhausted","unknown"]}
    }
  }' \
  "You just finished implementing a budget feature. Report your result."
# Validate all 7 required fields are present and correctly typed

# Test 3: Budget exhaustion mid-execution — capture exact error signal
claude -p --dangerously-skip-permissions --output-format json \
  --max-budget-usd 0.002 \
  --json-schema '{"type":"object","required":["story_id","status","files_changed","tests_run","test_passed","handoff_note","failure_type"]}' \
  "Explore every file in the crates/ directory, read each one in full, then report what you found." \
  > /tmp/budget_test_stdout.json 2> /tmp/budget_test_stderr.txt
echo "Exit code: $?"
cat /tmp/budget_test_stderr.txt
cat /tmp/budget_test_stdout.json
# Document: exit code, stderr content, stdout content (partial JSON? empty? error object?)
# This defines exactly what OpenFang checks to detect budget exhaustion
```

**Pass criteria:**
- [ ] Test 1: JSON output matches schema even when prompt explicitly asks for prose
- [ ] Test 2: All 7 required fields of the execute-phase schema present and correctly typed
- [ ] Test 3: Budget exhaustion produces consistent, parseable signal — document exact exit code and stderr pattern
- [ ] Test 3: No partial JSON written to stdout on budget exhaustion (verify output is either valid JSON or empty)
- [ ] Document findings → add exact error detection logic to US-008 acceptance criteria before implementation

**Unblocks:** US-005, US-008

---

### POC-3: Backlog API Integration

**Validates:** US-001, US-002, US-003b, US-004 — issue fetch, role classification, dependency detection  
**Risk if skipped:** Backlog API field names, status IDs, and webhook behaviour are all assumed. Any mismatch breaks core pipeline logic.

**Setup:** Replace `{SPACE}`, `{PROJECT_KEY}`, `{ISSUE_KEY}` with real values before running.

**Experiment:**
```bash
export BACKLOG_BASE="https://{SPACE}.backlog.com"
export API_KEY="$BACKLOG_API_KEY"

# Test 1: Confirm API reachability (used in pipeline doctor check)
curl -s "$BACKLOG_BASE/api/v2/projects?apiKey=$API_KEY" | jq '.[0] | {id, projectKey, name}'

# Test 2: Fetch issue list with filters (the actual polling query)
curl -s "$BACKLOG_BASE/api/v2/issues?apiKey=$API_KEY&projectId[]={PROJECT_ID}&statusId[]=1&statusId[]=2&order=priority&count=10" | jq '.[] | {id, issueKey, summary, status, priority, issueType, category}'
# Document: exact field names for status, category, issueType

# Test 3: Fetch a single issue — inspect ALL fields
curl -s "$BACKLOG_BASE/api/v2/issues/{ISSUE_KEY}?apiKey=$API_KEY" | jq .
# Document:
# - Exact field for related/parent issues (relatedIssues? parentIssueId? linked_issues?)
# - Exact statusId integers for: Open, In Progress, In Review, On Hold, Resolved
# - Category structure: is it an array? what fields?

# Test 4: Update status to "In Progress"
# First, get the correct statusId from Test 3 results
curl -s -X PATCH "$BACKLOG_BASE/api/v2/issues/{ISSUE_KEY}?apiKey=$API_KEY" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  -d "statusId={IN_PROGRESS_STATUS_ID}"
# Verify: check issue in Backlog UI — status must have changed

# Test 5: Add a comment
curl -s -X POST "$BACKLOG_BASE/api/v2/issues/{ISSUE_KEY}/comments?apiKey=$API_KEY" \
  -H "Content-Type: application/x-www-form-urlencoded" \
  --data-urlencode "content=Pipeline POC test comment - please ignore"
# Verify: comment appears in Backlog issue UI

# Test 6: Rate limit probe — make 10 rapid requests, check for 429 or headers
for i in $(seq 1 10); do
  curl -s -o /dev/null -w "%{http_code} " \
    "$BACKLOG_BASE/api/v2/issues/{ISSUE_KEY}?apiKey=$API_KEY"
done
echo ""
# Document: any 429s? X-RateLimit headers?

# Test 7: Webhook setup
# This is done via Backlog UI: Project Settings → Integrations → Webhook
# Set up a webhook pointing to: http://{pipeline_host}:4200/hooks/agent
# Trigger: "Issue Created" or "Issue Updated"
# Then move an issue status in Backlog UI and check OpenFang logs for the webhook POST
```

**Pass criteria:**
- [ ] All field names documented with exact JSON paths (status.id, issueType.name, category[].name, etc.)
- [ ] Dependency field identified and named exactly — update US-003b before implementation
- [ ] Correct statusId integers documented for all 5 states used by the pipeline
- [ ] Status PATCH confirmed working — verified in Backlog UI
- [ ] Comment POST confirmed working — verified in Backlog UI
- [ ] Rate limit behaviour documented (threshold, retry-after header if any)
- [ ] Webhook delivery to `/hooks/agent` confirmed — OpenFang receives the POST

**Unblocks:** US-001, US-002, US-003b, US-004

---

### POC-4: Guard Runner — Baseline Rules on Real Code

**Validates:** US-010, US-011 — guard accuracy and false positive rate  
**Risk if skipped:** Guards firing on bad patterns produce noise; guards missing real issues give false confidence.

**Note on `no_unwrap_production`:** `grep -v "#\[cfg(test)\]"` does NOT filter `.unwrap()` calls inside test modules — the `#[cfg(test)]` attribute is on a different line than the `.unwrap()` call. Use `grep -v "_test"` on the filename instead, accepting that in-file test modules are a known FP source (documented as warn not error in PRD).

**Experiment:**
```bash
cd /Users/rajesh/Documents/GitHub/openfang

echo "=== Timing: full repo guard run ==="
time (

echo "--- no_hardcoded_ports (strings containing 4-digit port values) ---"
grep -rn '"[0-9]\{4\}"' crates/ --include="*.rs" | grep -v "_test\|tests/" | grep -v "\.md"

echo "--- no_hardcoded_ips ---"
grep -rn '"[0-9]\{1,3\}\.[0-9]\{1,3\}\.[0-9]\{1,3\}\.[0-9]\{1,3\}"' crates/ --include="*.rs" --include="*.html" --include="*.js" | grep -v "_test\|tests/"

echo "--- no_unwrap_production (known FP: in-file #[cfg(test)] modules) ---"
grep -rn '\.unwrap()' crates/ --include="*.rs" | grep -v "_test\.rs\|tests/"
# Count and manually classify 10 samples as TP or FP

echo "--- no_todo_left_in_impl ---"
grep -rn 'todo!()\|unimplemented!()' crates/ --include="*.rs" | grep -v "_test\|tests/"

echo "--- no_magic_numbers (scoped to comparison operators only, not all integers) ---"
grep -rn '\(>\|<\|>=\|<=\|==\) [0-9]\{2,\}\b' crates/ --include="*.rs" | grep -v "_test\|tests/\|const \|==" | head -30
# Note: == excluded to avoid catching test assertions

echo "--- business_logic_in_routes ---"
grep -rn '\bif\b\|\bfor\b\|\bwhile\b\|\bmatch\b' crates/openfang-api/src/routes.rs 2>/dev/null | head -20

echo "--- no_credentials_in_code ---"
grep -rn -i '\(password\|secret\|api_key\)\s*=\s*"[^"]\{4,\}"' crates/ --include="*.rs"

)

# Test scope restriction: guards must run only on specific files, not all files
echo "=== Scope test: guard on single file only ==="
grep -n '\.unwrap()' crates/openfang-kernel/src/budget.rs 2>/dev/null || echo "(file not found or no matches)"
```

**Pass criteria:**
- [ ] Full guard run on entire `crates/` completes in < 5 seconds — time it
- [ ] For each rule: manually review first 10 matches, count FPs, document rate
- [ ] Rules with > 20% FP: either pattern refined or severity downgraded to `warn` — decisions recorded
- [ ] `no_unwrap_production` FP rate from in-file test modules measured and accepted as `warn`
- [ ] `no_magic_numbers` refined pattern confirmed — not firing on every array index or constant
- [ ] Guards confirmed to work on a single-file scope (not always full repo)
- [ ] Refined patterns documented → replace baseline patterns in PRD US-011 table before implementation

**Unblocks:** US-010, US-011

---

### POC-5: Repo Map Generation

**Validates:** Aider-inspired improvement to US-005 prompt assembly  
**Risk if skipped:** Without a repo map, Claude may hallucinate file locations or miss key symbol names.

**Note on ctags:** Standard `ctags` does not support `--output-format=json`. Use `universal-ctags` (brew install universal-ctags) or ripgrep. Ripgrep is the safer choice — always installed.

**Experiment:**
```bash
cd /Users/rajesh/Documents/GitHub/openfang

echo "=== Generate repo map via ripgrep ==="
# Correct pattern: match pub fn/struct/trait/enum anywhere on the line (not just line start)
rg "pub (fn|struct|trait|enum|impl)" crates/ --include="*.rs" -n \
  | grep -v "_test\|tests/" \
  | sed 's/:.*//' | sort -u \
  | head -5  # sample filenames

# Full map: file + line + symbol
rg "(^|\s)pub (fn|struct|trait|enum) \w+" crates/ --include="*.rs" -n \
  | grep -v "_test\|tests/" \
  > /tmp/repo_map_raw.txt
wc -l /tmp/repo_map_raw.txt
head -30 /tmp/repo_map_raw.txt

echo "=== Token count estimate ==="
# Rough estimate: ~1.3 tokens per word, ~5 chars per token
wc -c /tmp/repo_map_raw.txt
# If > 10000 chars (~2000 tokens), trim to top N lines
head -300 /tmp/repo_map_raw.txt | wc -c

echo "=== Test 1: Claude WITHOUT repo map ==="
claude -p --dangerously-skip-permissions \
  "Where is budget tracking implemented in this codebase? Give me the exact file path and function name." \
  > /tmp/no_map_response.txt
cat /tmp/no_map_response.txt

echo "=== Test 2: Claude WITH repo map ==="
REPO_MAP=$(head -300 /tmp/repo_map_raw.txt)
claude -p --dangerously-skip-permissions \
  "REPO MAP (public symbols):
$REPO_MAP

---
Where is budget tracking implemented? Give me the exact file path and function name." \
  > /tmp/with_map_response.txt
cat /tmp/with_map_response.txt

# Compare: did the map help Claude navigate faster and more accurately?
# Grade each response: correct file? correct function? hallucinated anything?
```

**Pass criteria:**
- [ ] Repo map generated in < 10 seconds
- [ ] Trimmed to 300 lines fits in < 2000 tokens (verify by char count / 5)
- [ ] Test 1 vs Test 2: Claude with map identifies correct file(s) more accurately — grade both responses
- [ ] If map does NOT improve accuracy: document why and remove from US-005 prompt structure
- [ ] Generation command finalised — add to `pipeline setup` spec in US-018

**Unblocks:** US-005 (repo map injection decision), US-018 (setup command)

---

### POC-6: OpenFang Workflow Engine as Pipeline State Machine

**Validates:** Research finding that existing Workflow Engine can replace custom state machine  
**Risk if skipped:** If the engine can't call external processes or pause for human gates, we need a custom state machine — a major architecture decision.

**Experiment:**
```bash
# Start OpenFang daemon first
cd /Users/rajesh/Documents/GitHub/openfang
cargo build --release -p openfang-cli 2>/dev/null
ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY target/release/openfang start &
sleep 6
curl -s http://127.0.0.1:4200/api/health

# Test 1: Create a 3-step sequential workflow
curl -s -X POST http://127.0.0.1:4200/api/workflows \
  -H "Content-Type: application/json" \
  -d '{
    "name": "poc-pipeline-test",
    "description": "POC: 3-step pipeline simulation",
    "steps": [
      {
        "name": "decompose",
        "agent": "default",
        "prompt_template": "Say the word STEP1_DONE and nothing else.",
        "mode": "Sequential",
        "output_var": "step1_result",
        "timeout_secs": 30
      },
      {
        "name": "implement",
        "agent": "default",
        "prompt_template": "Previous step said: {{step1_result}}. Now say STEP2_DONE.",
        "mode": "Sequential",
        "output_var": "step2_result",
        "timeout_secs": 30
      },
      {
        "name": "review",
        "agent": "default",
        "prompt_template": "Steps said: {{step1_result}} and {{step2_result}}. Say STEP3_DONE.",
        "mode": "Sequential",
        "timeout_secs": 30
      }
    ]
  }' | jq '{id, name}'

WORKFLOW_ID=$(curl -s http://127.0.0.1:4200/api/workflows | jq -r '.[] | select(.name=="poc-pipeline-test") | .id')

# Run it
curl -s -X POST "http://127.0.0.1:4200/api/workflows/$WORKFLOW_ID/run" | jq .

# Poll for result
sleep 10
curl -s http://127.0.0.1:4200/api/workflows/$WORKFLOW_ID/runs | jq '.[-1] | {status, outputs}'
# Pass: outputs contain STEP1_DONE, STEP2_DONE, STEP3_DONE with correct variable substitution

# Test 2: Can a workflow step invoke an external shell command / subprocess?
# Check the workflow step prompt_template for bash execution capability
# OR: check if agent tools include a Bash tool that the workflow step can invoke
curl -s http://127.0.0.1:4200/api/agents | jq '.[0] | {name, tools}'
# Key question: does the agent used by workflow steps have access to Bash/shell tools?

# Test 3: Can a workflow pause for human approval?
# Check approvals system
curl -s http://127.0.0.1:4200/api/approvals 2>/dev/null | jq .
# Try to submit an approval request via the kernel handle
# This may require reading approvals.rs to understand the API surface

# Cleanup
pkill -f "openfang start"
```

**Pass criteria:**
- [ ] 3-step workflow runs sequentially with `output_var` passing data between steps — verified in run output
- [ ] **Critical:** workflow step agent has Bash tool access (can run `claude -p` as subprocess) — YES or NO documented
- [ ] **Critical:** workflow can pause for human approval (approval system blocks step progression) — YES or NO documented
- [ ] If either critical test is NO: document exactly what is missing → custom state machine scope defined

**Decision:** If both critical tests pass → use workflow engine. If either fails → build minimal custom state machine for those specific gaps only, wrap the rest in workflow engine.

**Unblocks:** Architecture decision (scoped to the gap, not rebuild-everything)

---

### POC-7: Git Worktree Isolation with Claude CLI

**Validates:** Chief-inspired worktree isolation, US-003 branch model  
**Risk if skipped:** Claude CLI might not discover `CLAUDE.md` from a worktree, or git operations may behave unexpectedly.

**Note on CLAUDE.md in worktrees:** A git worktree checks out the branch's committed tree. `CLAUDE.md` will only be present if it is committed to the branch the worktree checks out. If `CLAUDE.md` is only on `main`/`dev` and the new `pipeline/*` branch branches from `agent/dev`, `CLAUDE.md` will be present only if `agent/dev` has it committed. This must be verified — it's not guaranteed.

**Experiment:**
```bash
cd /Users/rajesh/Documents/GitHub/openfang

# Verify CLAUDE.md is committed (not just present as untracked)
git ls-files CLAUDE.md
# If output is empty: CLAUDE.md is untracked → it will NOT appear in worktrees
# Resolution: either commit CLAUDE.md or pass it via --system-prompt-file flag instead

# Create worktree (note: git worktree remove must come BEFORE git branch -D)
git worktree add /tmp/poc-worktree pipeline/OFANG-WTEST
ls /tmp/poc-worktree/CLAUDE.md && echo "CLAUDE.md present" || echo "CLAUDE.md MISSING — worktree isolation breaks prompt assembly"

# Run Claude from worktree
cd /tmp/poc-worktree
claude -p --dangerously-skip-permissions \
  "What branch are you on? List the files in the current directory. Is there a CLAUDE.md file?"

# Test commit from worktree
claude -p --dangerously-skip-permissions \
  "Create a file poc_worktree_test.txt with content 'worktree works', stage it, and commit with message 'poc: worktree commit test'"
git log --oneline -3

# Test push from worktree to correct branch
git push -u origin pipeline/OFANG-WTEST
git log --oneline origin/pipeline/OFANG-WTEST -1
# Pass: commit appears on remote branch, not on main or agent/dev

# Test sessions are isolated: Claude session started in worktree vs main directory
SESSION_IN_WORKTREE=$(claude -p --output-format json --dangerously-skip-permissions \
  "Say 'worktree session'" | jq -r '.session_id')
cd /Users/rajesh/Documents/GitHub/openfang
SESSION_IN_MAIN=$(claude -p --output-format json --dangerously-skip-permissions \
  "Say 'main session'" | jq -r '.session_id')
echo "Worktree session: $SESSION_IN_WORKTREE"
echo "Main session: $SESSION_IN_MAIN"
# Are they different? Can you --resume a worktree session from the main directory?

# Cleanup (MUST remove worktree before deleting branch)
git worktree remove /tmp/poc-worktree
git branch -D pipeline/OFANG-WTEST
git push origin --delete pipeline/OFANG-WTEST
```

**Pass criteria:**
- [ ] `CLAUDE.md` committed status verified — if untracked, document resolution (commit it OR use `--system-prompt-file`)
- [ ] `CLAUDE.md` accessible from worktree when the branch has it committed
- [ ] Claude CLI runs normally from worktree — branch name, file list correct
- [ ] Commit from worktree lands on `pipeline/OFANG-WTEST`, not on any other branch
- [ ] Push from worktree goes to correct remote branch
- [ ] Sessions are independent between worktree and main checkout
- [ ] Cleanup is clean: `git worktree list` shows no leftover entries

**Unblocks:** US-003 (worktree-per-issue decision), US-005 (CLAUDE.md injection strategy)

---

### POC-8: End-to-End Draft PR to `agent/dev`

**Validates:** US-003 (branch model), US-017 (PR creation), `gh` CLI integration  
**Risk if skipped:** `gh` auth in pipeline context, `agent/dev` branch existence, PR targeting — any of these can silently fail.

**Experiment:**
```bash
cd /Users/rajesh/Documents/GitHub/openfang

# Pre-check: gh is authenticated (no interactive prompts expected)
gh auth status
# If not authenticated: gh auth login — do this once before the pipeline is used

# Step 1: Create agent/dev from base branch (idempotent)
# Note: Replace 'dev' with your repo's {base_branch} value from .pipeline/config.toml
git checkout dev 2>/dev/null || git checkout main
git pull
git checkout agent/dev 2>/dev/null || git checkout -b agent/dev
git push -u origin agent/dev 2>/dev/null || echo "agent/dev already on remote"

# Step 2: Create pipeline issue branch from agent/dev
git checkout agent/dev
git checkout -b pipeline/OFANG-TEST-001

# Step 3: Make a trivial change and commit
mkdir -p PIPELINE
echo "# pipeline POC test — safe to delete" > PIPELINE/TEST.md
git add PIPELINE/TEST.md
git commit -m "OFANG-TEST-001: pipeline draft PR test"
git push -u origin pipeline/OFANG-TEST-001

# Step 4: Open DRAFT PR targeting agent/dev (not main, not dev)
PR_URL=$(gh pr create \
  --draft \
  --base agent/dev \
  --head pipeline/OFANG-TEST-001 \
  --title "OFANG-TEST-001: pipeline draft PR test" \
  --body "POC test — verifying pipeline branch model. Safe to close.")
echo "PR URL: $PR_URL"
PR_NUM=$(echo "$PR_URL" | grep -oE '[0-9]+$')

# Step 5: Verify PR targets agent/dev and is draft
gh pr view "$PR_NUM" --json baseRefName,headRefName,isDraft,url | jq .
# Expected: baseRefName = "agent/dev", isDraft = true

# Step 6: Convert draft to ready (simulates all stories approved)
gh pr ready "$PR_NUM"
gh pr view "$PR_NUM" --json isDraft | jq '.isDraft'
# Expected: false

# Step 7: Merge PR into agent/dev and verify chained branch picks up changes
gh pr merge "$PR_NUM" --merge --delete-branch
git fetch origin
git checkout agent/dev && git pull

# Now create a second "chained" issue branch from updated agent/dev
git checkout -b pipeline/OFANG-TEST-002
git log --oneline -3
# Verify: OFANG-TEST-001's commit is in the history of OFANG-TEST-002

# Step 8: Full cleanup
# Note: Replace 'dev' with your repo's {base_branch} value from .pipeline/config.toml
git checkout dev 2>/dev/null || git checkout main
git branch -D pipeline/OFANG-TEST-002
git push origin --delete pipeline/OFANG-TEST-002 2>/dev/null || true
```

**Pass criteria:**
- [ ] `gh auth status` passes without interactive prompt — confirmed pre-authenticated
- [ ] `agent/dev` branch created from base branch and pushed to remote
- [ ] `pipeline/OFANG-TEST-001` branched from `agent/dev`
- [ ] Draft PR opens with `baseRefName: agent/dev` — verified via `gh pr view`
- [ ] `gh pr ready` converts to non-draft — `isDraft: false` confirmed
- [ ] After merge, `pipeline/OFANG-TEST-002` (chained issue) contains OFANG-TEST-001's commit in its history — chained dependency model works end-to-end
- [ ] No interactive auth prompts at any step — pipeline can run unattended

**Unblocks:** US-003, US-003b, US-017 — full branch + chained dependency model verified

---

### POC-9: Full Prompt Assembly → Claude Response Quality

**Validates:** US-005 — the core of the entire system: does passing CLAUDE.md + role standards + issue to `claude -p` produce a grounded, correct decomposition?  
**Risk if skipped:** All other POCs are irrelevant if Claude can't produce a useful plan from a correctly assembled prompt. This is the foundation.

**Experiment:**
```bash
cd /Users/rajesh/Documents/GitHub/openfang

# Assemble the exact prompt OpenFang will build (US-005 structure)
cat > /tmp/poc9_prompt.md << 'PROMPT_EOF'
$(cat CLAUDE.md)

---

$(cat .pipeline/backend.md 2>/dev/null || echo "# Backend Standards\n- Run: cargo test -p {affected_crate}\n- No unwrap() in production\n- No hardcoded ports or IPs")

---

BACKLOG ISSUE: OFANG-POC-001
Summary: Add per-agent token usage tracking to the budget system
Description: |
  Currently the budget system tracks global spend but not per-agent spend.
  We need to track tokens used per agent per session so that we can:
  1. Show per-agent cost breakdowns in the dashboard
  2. Enforce per-agent budgets separately from global budget
  Category: backend
  Priority: high

---

CURRENT PHASE: PLAN

Read the issue above. Explore the codebase to understand the current budget
and metering implementation. Break this into user stories if needed.
Write the plan to PIPELINE/PLAN-OFANG-POC-001.md.

Each story must have:
- A testable acceptance criterion
- An exact test scope command (cargo test -p {crate} {filter})

Then output only this JSON:
{"stories":[{"id":"US-001","title":"...","depends_on":[]}],"session_note":"..."}
PROMPT_EOF

# Evaluate: does the assembled prompt produce output that:
# 1. References real files (not invented paths)
# 2. Identifies the correct crate (openfang-kernel, metering.rs)
# 3. Produces acceptance criteria that are actually testable
# 4. Produces a test scope that targets the right crate

claude -p --dangerously-skip-permissions \
  --output-format json \
  --max-budget-usd 1.00 \
  --json-schema '{"type":"object","required":["stories","session_note"]}' \
  < /tmp/poc9_prompt.md \
  > /tmp/poc9_output.json

echo "=== Stories produced ==="
cat /tmp/poc9_output.json | jq '.stories[] | {id, title}'

echo "=== Plan file written? ==="
cat PIPELINE/PLAN-OFANG-POC-001.md 2>/dev/null || echo "PLAN FILE NOT WRITTEN"

# Grade the plan:
# - Does it reference crates/openfang-kernel/src/metering.rs? (real file)
# - Does test scope say `cargo test -p openfang-kernel`? (correct)
# - Are acceptance criteria measurable?

# Cleanup
rm -f PIPELINE/PLAN-OFANG-POC-001.md
```

**Pass criteria:**
- [ ] Plan file written to `PIPELINE/PLAN-OFANG-POC-001.md` before JSON output
- [ ] Plan references `crates/openfang-kernel/src/metering.rs` or similar real path — no invented files
- [ ] Test scope in plan uses `cargo test -p openfang-kernel` — correct crate identified
- [ ] Acceptance criteria are specific and testable (not "improve performance")
- [ ] JSON output parses against the schema with all required fields
- [ ] If plan quality is poor: diagnose whether CLAUDE.md content or prompt structure needs adjustment before any other story is built

**Unblocks:** US-005, US-006 — validates prompt assembly is sufficient for Claude to navigate correctly

---

### POC-10: OpenFang Approval System as External Gate

**Validates:** US-012 — human gates use OpenFang's existing approval system  
**Risk if skipped:** If `request_approval()` can't be called from an external process or doesn't block correctly, the gate design has no implementation path.

**Experiment:**
```bash
# Start OpenFang daemon
cd /Users/rajesh/Documents/GitHub/openfang
ANTHROPIC_API_KEY=$ANTHROPIC_API_KEY target/release/openfang start &
sleep 6
curl -s http://127.0.0.1:4200/api/health

# Test 1: Check approvals API surface
curl -s http://127.0.0.1:4200/api/approvals 2>/dev/null | jq . || echo "No approvals endpoint"
# Document: is there a REST endpoint to POST an approval request?

# Test 2: Check how approvals appear in the dashboard
# Open browser to http://127.0.0.1:4200
# Look for the approval badge on the Agents tab

# Test 3: Submit an approval request via API (if endpoint exists)
curl -s -X POST http://127.0.0.1:4200/api/approvals \
  -H "Content-Type: application/json" \
  -d '{
    "agent_id": "pipeline-orchestrator",
    "tool_name": "story_gate",
    "summary": "US-001 complete. 3 files changed. Tests passed. Approve to continue to US-002?"
  }' | jq .

# Test 4: Does the approval request block until answered?
# In one terminal: POST the approval request
# In another terminal/browser: approve it
# Measure: does the first terminal unblock?

# Test 5: If no REST endpoint — what is the approval mechanism?
# Read the approvals implementation
grep -n "request_approval\|approval" crates/openfang-kernel/src/approvals.rs | head -30

pkill -f "openfang start"
```

**Pass criteria:**
- [ ] Approval API endpoint exists and accepts requests from external process (pipeline binary)
- [ ] Submitted approval appears in dashboard badge count
- [ ] Approving via dashboard (or API) unblocks the waiting process — confirmed with timing
- [ ] If no blocking API: document the actual mechanism → update US-012 to match what actually exists
- [ ] Rejection via dashboard triggers correct response (not just timeout)

**Unblocks:** US-012 (approval gate implementation approach)

---

### POC-11: progress.md Accumulation and Cross-Issue Injection

**Validates:** Chief-inspired `progress.md` pattern — does accumulated codebase knowledge improve Claude's accuracy on later issues?  
**Risk if skipped:** `progress.md` is central to session recovery and token efficiency. If it doesn't help (or hurts by exceeding token budget), it should be cut before building it.

**Experiment:**
```bash
cd /Users/rajesh/Documents/GitHub/openfang

# Simulate 3 stories worth of progress.md content
cat > /tmp/test_progress.md << 'EOF'
# Codebase Progress Notes

## Patterns Discovered
- Budget tracking is in crates/openfang-kernel/src/metering.rs
- MeteringEngine struct holds UsageStore (SQLite-backed)
- New agent costs are tracked via record_usage(agent_id, model, tokens, cost)
- All API routes go through AppState in crates/openfang-api/src/routes.rs
- New routes: register in server.rs router AND implement handler in routes.rs
- Tests for kernel features: cargo test -p openfang-kernel {module_name}

## File Dependencies
- Changing AgentInfo struct → must update comms.rs topology response
- Adding KernelConfig fields → must add to Default impl in kernel.rs

## Test Quirks
- openfang-kernel tests require no daemon running (port conflict)
- cargo test -p openfang-api requires build first (generated types)
EOF

# Test 1: Claude WITHOUT progress.md on a follow-up issue
claude -p --dangerously-skip-permissions \
  --max-budget-usd 0.50 \
  "$(cat CLAUDE.md)

---

BACKLOG ISSUE: OFANG-TEST-FOLLOW
Summary: Add cost display to the analytics dashboard tab

What files need to change and what cargo test command should I run?" \
  > /tmp/no_progress_response.txt
cat /tmp/no_progress_response.txt

# Test 2: Claude WITH progress.md on same issue
claude -p --dangerously-skip-permissions \
  --max-budget-usd 0.50 \
  "$(cat CLAUDE.md)

---

CODEBASE NOTES (accumulated from previous issues):
$(cat /tmp/test_progress.md)

---

BACKLOG ISSUE: OFANG-TEST-FOLLOW
Summary: Add cost display to the analytics dashboard tab

What files need to change and what cargo test command should I run?" \
  > /tmp/with_progress_response.txt
cat /tmp/with_progress_response.txt

# Grade: does the progress.md response reference routes.rs + server.rs correctly?
# Does it give the right test command without re-discovering it?
# Token overhead: how many tokens does progress.md add?
wc -c /tmp/test_progress.md  # chars / 5 ≈ tokens
```

**Pass criteria:**
- [ ] Claude with `progress.md` references correct files (routes.rs, server.rs) without exploring first
- [ ] Test command is more accurate in the `progress.md` version
- [ ] `progress.md` content at 3 stories fits in < 800 tokens — measured
- [ ] If no measurable improvement: remove `progress.md` injection from US-005 (YAGNI)

**Unblocks:** US-005 (progress.md injection decision), US-016 (handoff mechanism)

---

### POC Summary Table

| POC | What it validates | Est. time | Blocks stories | Run order |
|-----|------------------|-----------|----------------|-----------|
| **POC-9** | Prompt assembly → Claude plan quality (foundation) | 1 hour | US-005, US-006 | **1st** |
| **POC-1** | `--resume` continuity + cross-dir + post-commit resume | 3 hrs (wait) | US-008, US-015, US-016 | **1st** |
| **POC-2** | `--json-schema` enforcement + budget exhaustion signal | 1 hour | US-005, US-008 | **1st** |
| **POC-8** | Draft PR → `agent/dev` + chained branch picks up changes | 1 hour | US-003, US-017 | **1st** |
| **POC-3** | Backlog API — real fields, statusIds, webhook delivery | 2 hours | US-001–US-004 | **2nd** |
| **POC-6** | Workflow Engine: subprocess + gate blocking capability | 3 hours | Architecture | **2nd** |
| **POC-10** | OpenFang approval system — external gate blocking | 2 hours | US-012 | **2nd** |
| **POC-7** | Claude CLI in worktree — CLAUDE.md, commits, sessions | 1 hour | US-003 | **3rd** |
| **POC-4** | Guard FP rate on real OpenFang code, pattern tuning | 2 hours | US-010, US-011 | **3rd** |
| **POC-5** | Repo map — size, accuracy vs no-map baseline | 2 hours | US-005, US-018 | **3rd** |
| **POC-11** | progress.md injection — measurable accuracy improvement | 1 hour | US-005, US-016 | **3rd** |

**Total time:** ~19 hours of active work + waiting time for POC-1 (overnight).  
Run 1st-order POCs before writing any implementation code. Run 2nd-order before finalising architecture. Run 3rd-order before implementing those specific stories.

---

## Glossary

| Term | Definition |
|------|-----------|
| `agent/dev` | Long-lived integration branch created once per repo. All approved pipeline PRs target this branch. Promotion to `{base_branch}` is a manual human action. Always named `agent/dev` regardless of `base_branch`. |
| `base_branch` | The repo's main working branch — `dev` for most repos, `main` for repos without a dev branch. Configured in `.pipeline/config.toml`. The pipeline never pushes directly to `base_branch`. |
| `pipeline/{issueKey}` | Short-lived per-issue branch, created from `agent/dev` HEAD. Deleted after its PR merges to `agent/dev`. |
| `PIPELINE/STATE-{key}.json` | Crash-safe state file written by the pipeline binary. Survives process kill. Contains: issue key, role, branch, session_id, phase, story progress, cycle counts. Read by `pipeline resume`. |
| `handoff_note` | Required JSON field in every execute-phase response. One paragraph — what was done, files changed, key decisions, what next story needs to know. Crash-safe fallback when `--resume` fails. |
| `progress_update` | Required JSON field in execute-phase response. Structured codebase patterns discovered this story (gotchas, file dependencies, test quirks). Accumulated by OpenFang into `PIPELINE/PROGRESS.md` and prepended to future prompts. |
| `failure_type` | Required JSON field: `none \| test_failure \| compilation_error \| timeout \| budget_exhausted \| unknown`. Drives OpenFang's retry/escalate decision. |
| Ralph Wiggum Loop | Pattern from [Chief](https://minicodemonkey.github.io/chief/). Fresh Claude session per story batch; accumulated progress notes (`progress.md`) prevent re-discovery without re-reading the same files. Prevents context overflow on large issues. |
| Gate | Human decision point at each story boundary: [A] Approve, [F] Flag (send back with feedback), [R] Reject (undo commit), [P] Pause. No story runs past its gate without human approval. |
| Cycle | One attempt to complete a story. Cycle count increments on each [F] or [R] action. Max 3 cycles before pipeline escalates to human. |
| Guard | Grep-based pattern check run on `files_changed` after every story execution. No LLM — fast and deterministic. Returns `error` (blocks gate) or `warn` (shown, does not block). |
| Repo map | ~1–2k token snapshot of public symbols in the codebase, generated by ripgrep. Prepended to every prompt so Claude navigates multi-file changes accurately. |
| `CLAUDE.md` | Checked-in file at repo root documenting codebase structure, conventions, test patterns, API contracts, and gotchas. Primary repo knowledge for Claude. The pipeline hard-stops if this file is missing. |
| `--dangerously-skip-permissions` | Claude CLI flag that suppresses per-tool approval prompts. Required for non-interactive pipeline operation. Only use in trusted pipeline context — never in untrusted environments. |
| `--json-schema` | Claude CLI flag that enforces structured JSON output. Claude's response must match the provided schema or the call fails. Not instruction-dependent — deterministic enforcement. |
| `session_id` | Identifier returned by Claude CLI on first call. Used with `--resume {session_id}` on subsequent calls to restore conversation context. Expires after some period (validated in POC-1). |
| Daemon mode | Pipeline running as a background process (no terminal attached), polling Backlog continuously. Logs go to file. Managed via `pipeline start --daemon` / `pipeline stop`. |

---

## Implementation Phases

> Stories are grouped by functional area in the User Stories section. This table shows the order to implement them.

| Phase | Stories | Goal | Gate to proceed |
|-------|---------|------|-----------------|
| **Phase 0 — POCs** | POC-1, POC-2, POC-8, POC-9 (1st order) | Validate core technical assumptions | All 4 POCs pass |
| **Phase 1 — Skeleton** | US-000, US-018, US-003, US-005 | Auth + setup + branching + prompt assembly | Pipeline can assemble and send one prompt |
| **Phase 2 — Decompose loop** | US-001, US-002, US-006, US-007 | Fetch issue → classify → plan → human gate 1 | Human can approve/reject a decomposed plan |
| **Phase 3 — Execute loop** | US-008, US-009, US-010, US-011, US-012 | Run stories → guards → human gate | Full story cycle works end-to-end on a real issue |
| **Phase 4 — Feedback loop** | US-013, US-014, US-015, US-016 | Flag/reject/pause/resume | Human can control all cycle outcomes |
| **Phase 5 — Completion** | US-004, US-003b, US-017 | Backlog updates + dependency checks + PR | Issue goes from Backlog to merged PR |
| **Phase 6 — Observability** | US-019, US-020 | Dashboard tab + local/server/daemon deployment modes | Pipeline visible in Tauri app, runnable on server via SSH |

**Minimum shippable v1:** Phases 0–4. Phase 5 adds Backlog polish. Phase 6 adds observability.

---

## Success Criteria for v1

The pipeline is considered working when:

1. **5 real issues** (mix of backend and frontend) run end-to-end: Backlog → plan → stories → guards → approval gates → PR on `agent/dev`
2. **No manual intervention** needed between Gate 1 (plan approval) and PR creation on the happy path
3. **All 4 first-order POCs pass** before implementation begins
4. **Per-issue cost** stays below $5.00 on typical 3–5 story issues
5. **Crash recovery works:** kill the pipeline mid-story, run `pipeline resume`, verify it picks up from the correct story
6. **Guards catch at least one real violation** on a real codebase run before the human sees it

---

## User Stories

---

### GROUP 0 — Startup and Auth

---

---

### US-000: Validate Claude CLI authentication on startup

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
Before touching any Backlog issue, the pipeline verifies Claude CLI is authenticated. Auth is stored in `~/Library/Application Support/Claude/config.json` (macOS) after running `claude setup-token` once in any terminal. The pipeline reads from the same location — no shell-to-shell passing needed.

**Two auth modes:**

| Mode | Setup | Best for |
|------|-------|----------|
| `claude setup-token` | Human runs once in terminal → browser OAuth → long-lived token stored on disk | Subscription users, persistent daemon |
| `ANTHROPIC_API_KEY` env var | Set in environment before starting pipeline | API key users, CI-like environments |

**Acceptance Criteria:**
- [ ] On startup, pipeline runs: `claude -p --max-budget-usd 0.001 "reply with the word ready"` as auth probe
- [ ] If probe succeeds: continue. If it fails with auth error: stop with message:
  ```
  Claude CLI is not authenticated.
  Run one of:
    claude setup-token          (recommended for pipeline use — long-lived)
    export ANTHROPIC_API_KEY=…  (API key alternative)
  Then restart the pipeline.
  ```
- [ ] Pipeline never proceeds past startup without a passing auth probe
- [ ] Auth mode detected and logged: "Auth: setup-token" or "Auth: ANTHROPIC_API_KEY"
- [ ] Auth probe result cached for the session — not re-checked per issue
- [ ] If `ANTHROPIC_API_KEY` is set, it takes precedence over stored token (Claude CLI behaviour)
- [ ] On every startup, before fetching any Backlog issue, pipeline automatically runs three checks in sequence and reports all three:
  1. Claude CLI auth probe (`claude -p "ready"`)
  2. Backlog API reachability (`GET /api/v2/projects`)
  3. `gh auth status` (for PR creation)
- [ ] If all three pass: pipeline continues automatically — no user prompt needed
- [ ] If any fail: pipeline stops, prints exactly what failed and the fix command, exits
- [ ] `pipeline doctor` as an explicit standalone command re-runs the same three checks on demand without starting the pipeline

**Test scope:** unit test auth probe parsing; manual test both auth modes

---

### GROUP 1 — Backlog Integration

---

### US-001: Fetch and queue issues from Backlog

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
OpenFang polls the Backlog API, fetches open issues for the configured project, and selects the highest priority issue not already in the pipeline.

**Acceptance Criteria:**
- [ ] Reads `BACKLOG_API_KEY` from environment — pipeline will not start without it
- [ ] Reads `backlogBase` and `backlogProject` from `.pipeline/config.toml`
- [ ] Fetches open issues ordered by priority descending via `GET /api/v2/issues`
- [ ] Skips issues with status In Progress (`id=2`), Resolved (`id=3`), In-QA (`id=30576`), Reopen (`id=30577`), Need Info (`id=30612`), or Closed (`id=4`) — confirmed via POC-3
- [ ] Poll query: `GET /api/v2/issues?statusId[]=1&sort=priority&order=asc&count=10` — Open only, High priority first
- [ ] Skips issues that have an active `PIPELINE/STATE-{key}.json` in the repo
- [ ] Selects one issue at a time — no parallel execution in v1
- [ ] On selection, updates issue status to "In Progress" (`statusId=2`) via `PATCH /api/v2/issues/{issueKey}`
- [ ] Polling interval configurable in `config.toml` (default: 5 minutes). Webhook upgrade deferred to v2 (no public URL needed for v1 polling).
- [ ] Backlog API errors: retry up to 3 times with exponential backoff (30s, 60s, 120s). If all retries fail: log error, skip poll cycle, try again next interval — do not crash pipeline
- [ ] Stale state recovery: on startup, check for any issue whose `PIPELINE/STATE-{key}.json` shows `phase != complete` AND `last_updated > 24 hours ago`. If found: add Backlog comment "Pipeline may have crashed on this issue. Run `pipeline resume {key}` or `pipeline abandon {key}`." Revert status to Open (`statusId=1`)

**Test scope:** unit tests with mocked Backlog HTTP responses

---

### US-002: Classify issue role from metadata

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
Determine backend / frontend / fullstack from issue category and type labels. Deterministic lookup — no LLM call.

**Acceptance Criteria:**
- [ ] Label → role mapping defined in `.pipeline/config.toml` under `[roles]`
- [ ] Matches against `issueType.name` first, then `category` names if present (POC-3: ONCACCESS has no categories — `issueType` is the primary discriminator), then keyword scan of `summary` (case-insensitive)
- [ ] If no label matches: pipeline skips the issue and adds Backlog comment: "Pipeline skipped: issue has no backend or frontend label. Add a role label to process this issue." — never silently classifies as fullstack
- [ ] **Fullstack issues rejected in v1:** if an issue has both backend and frontend labels, pipeline skips it with Backlog comment: "Pipeline skipped: fullstack issues not supported in v1. Split into a backend issue and a frontend issue in Backlog." This is not a design gap — it is a deliberate v1 constraint.
- [ ] Classification logged to console: "Role classified as: backend (matched label: api)"
- [ ] Correct role file selected: `.pipeline/backend.md` or `.pipeline/frontend.md`
- [ ] Classification stored in `PIPELINE/STATE-{key}.json`

**Example config:**
```toml
[roles]
backend  = ["backend", "api", "kernel", "rust", "database", "performance"]
frontend = ["frontend", "ui", "dashboard", "ux", "html", "css"]
# anything not matching → fullstack
```

**Test scope:** unit tests for classifier — test each label pattern, test fallback

---

### US-003: Branch management — per-issue branches off agent/dev

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
The pipeline uses a two-level branch model. `agent/dev` is the long-lived integration branch — all approved issue work accumulates here. Each issue gets a short-lived `pipeline/{issueKey}` branch off `agent/dev`. PRs target `agent/dev`, not the repo's base branch (`main` or `dev`). Promotion from `agent/dev` → repo base branch is a deliberate human action.

**Base branch** is configurable per repo — most repos work off `dev`, some off `main`. `agent/dev` always branches from this configured base.

**Branch lifecycle:**
```
{base_branch}  (main or dev — configured per repo)
      ↓ agent/dev branches from here (once, on first pipeline run)
      agent/dev  (long-lived — accumulates all approved work)
            ↓ per issue
            pipeline/OFANG-123  (short-lived — deleted after merge to agent/dev)
```

**Acceptance Criteria:**
- [ ] Base branch configurable in `.pipeline/config.toml`: `base_branch = "dev"` (default: `"dev"`)
- [ ] `agent/dev` created from `{base_branch}` if it does not exist; checked out if it does
- [ ] `agent/dev` synced with `{base_branch}` before each new issue: `git merge {base_branch} --no-edit`
- [ ] If sync merge fails due to conflict: pipeline stops with error "Cannot sync agent/dev with {base_branch} — merge conflict. Resolve manually, then run `pipeline resume {key}`." Pipeline does NOT attempt auto-resolve.
- [ ] Per-issue branch: `pipeline/{issueKey}` created from current `agent/dev` HEAD — always AFTER sync completes
- [ ] **Git worktree created for every issue** (POC-1/POC-7 finding — required for session continuity):
  ```bash
  git worktree add ~/.pipeline/worktrees/{issueKey}/ pipeline/{issueKey}
  ```
  The pipeline binary's working directory for ALL Claude calls is `~/.pipeline/worktrees/{issueKey}/`. Sessions are scoped to this directory — never change cwd between calls.
- [ ] If worktree already exists at that path (resume): reuse without recreate
- [ ] Pipeline stops with clear error if working tree has uncommitted changes on startup
- [ ] STATE file includes `worktree_path` field for resume: `~/.pipeline/worktrees/{issueKey}/`
- [ ] After PR merged to `agent/dev`: worktree removed (`git worktree remove`) then branch deleted
- [ ] Human shown on each branch operation:
  ```
  Synced agent/dev with dev (+3 commits)
  Created pipeline/OFANG-123 from agent/dev
  Worktree: ~/.pipeline/worktrees/OFANG-123/
  ```

**Test scope:** unit tests for branch creation, sync, worktree creation, resume, and cleanup logic

---

### US-003b: Detect and respect chained issue dependencies

**Priority:** 2  
**Owner:** OpenFang

**Description:**  
Some Backlog issues depend on others — OFANG-102 cannot start until OFANG-101 is merged into `agent/dev`. The pipeline detects this from Backlog's related issue links and waits for the dependency to clear before branching.

**Acceptance Criteria:**
- [ ] On issue fetch: read `parentIssueId` from Backlog API response — confirmed via POC-3 as the dependency field (no `relatedIssues` field exists; `parentIssueId` is the only built-in dependency)
- [ ] If `parentIssueId` is non-null: resolve to parent `issueKey` via `GET /api/v2/issues/{parentIssueId}`
- [ ] Cross-reference with pipeline's own state: is the dependency's PR merged into `agent/dev`?
- [ ] Dependency check: `git log agent/dev --grep="{depIssueKey}" --oneline` — if found, dependency is satisfied
- [ ] If dependency not yet merged: skip this issue, log "OFANG-102 waiting on OFANG-101 (not yet in agent/dev)", pick next available issue
- [ ] Dependency check re-evaluated on every polling cycle
- [ ] Backlog comment added when issue is skipped due to dependency: "Waiting for OFANG-101 to merge before starting"
- [ ] When dependency clears: pipeline picks up the waiting issue on next poll cycle

**Test scope:** manual test with two related issues; verify OFANG-102 does not start before OFANG-101 merges

---

### US-004: Update Backlog issue with pipeline progress

**Priority:** 2  
**Owner:** OpenFang

**Description:**  
At key pipeline events, update the Backlog issue status and add a comment. Team sees progress without checking the terminal.

**Acceptance Criteria:**
- [ ] On pipeline start → status: "In Progress", comment: "Pipeline started"
- [ ] On Gate 1 approval → comment: "Plan approved: N stories"
- [ ] On each story approval → comment: "US-00N complete and approved"
- [ ] On story rejection (cycle 3) → comment: "US-00N needs human intervention", status: "On Hold"
- [ ] On blocker → comment: reason text, status: "On Hold"
- [ ] On pause → comment: "Pipeline paused — resume with `pipeline resume {key}`"
- [ ] On PR created → comment: PR URL, status: "In Review"
- [ ] Backlog API failures are logged but never crash the pipeline — work continues

**Test scope:** unit tests with mocked Backlog API for each event type

---

### GROUP 2 — Prompt Assembly (Core OpenFang Value)

---

### US-005: Assemble the correct prompt for each phase

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
OpenFang's primary job. For every Claude CLI call, assemble the prompt in the correct order: CLAUDE.md → role standards → issue → phase instruction. This is what makes Claude effective — the prompt must be complete, correctly ordered, and contain no contradictions.

**Acceptance Criteria:**
- [ ] Prompt always follows this exact order (POC-9 + POC-11 confirmed):
  1. Full contents of `CLAUDE.md` from the target repo
  2. Contents of `PIPELINE/PROGRESS.md` if it exists (mandatory after first story — see US-016; POC-11: 38% cost reduction)
  3. Full contents of `.pipeline/backend.md` or `.pipeline/frontend.md`
  4. Backlog issue (key, summary, description, category, priority)
  5. Phase-specific instruction block (changes per phase)
  6. *(Optional)* Repo map: first 100 lines from `rg "(^|\s)pub (fn|struct|trait|enum)" crates/ -g "*.rs" -n` — only if `repo_map_lines > 0` in `config.toml` (default: 0 = disabled; POC-5: marginal benefit on known codebases)
- [ ] `CLAUDE.md` not found → pipeline stops with clear error: "No CLAUDE.md found at {path}. Run `pipeline setup` first."
- [ ] Role standards file not found → pipeline stops with clear error
- [ ] Prompt written to `PIPELINE/PROMPT-{key}-{phase}.md` for debugging and audit
- [ ] Prompt size logged before each call: "Prompt: ~{N} tokens estimated (CLAUDE.md: Xk, progress: Yk, role: Zk)"
- [ ] If estimated prompt > 60k tokens: warn human before proceeding
- [ ] Decompose phase cost baseline: ~$0.20–0.30 per issue (POC-9). If a call exceeds $0.50: log warning "Prompt may be too large"

**Phase instruction blocks:**

*Decompose phase:*
```
PHASE: PLAN

Read the issue above. Explore the codebase to understand scope.
Break this into user stories if it is large (> one concern).
Keep each story to 15-30 minutes of focused work.
Write the plan to PIPELINE/PLAN-{issueKey}.md.

Story format:
### US-001: {verb + outcome}
**Depends on:** none | US-00X
**Description:** one sentence
**Acceptance Criteria:**
- [ ] testable criterion
**Test scope:** exact command (e.g. cargo test -p crate-name filter)

Then output only this JSON:
{"stories": [{"id":"US-001","title":"...","depends_on":[]}], "session_note": "key context for subsequent stories"}
```

*Execute phase (per story):*
```
PHASE: IMPLEMENT US-00N

The plan is at PIPELINE/PLAN-{issueKey}.md.
Completed stories: {list}
Session context: {session_note}

Implement US-00N only. Do not touch other stories.
Run only the test scope defined for this story in the plan.
Fix test failures before reporting completion.
Commit with message: "{issueKey} US-00N: {title in imperative}"

Then output only this JSON:
{"story_id":"US-00N","status":"done|blocked","blocker":null,"files_changed":[...],"tests_run":[...],"test_passed":true}
```

*Gap fix phase (after flag/reject):*
```
PHASE: FIX US-00N

Reviewer feedback:
{feedback text}

Address this feedback in the current implementation.
Re-run the story's test scope.
Amend the commit for US-00N.

Then output only this JSON:
{"story_id":"US-00N","status":"done|blocked","blocker":null,"files_changed":[...],"tests_run":[...],"test_passed":true}
```

*PR phase:*
```
PHASE: OPEN PR

All stories complete. Completed: {list of US-IDs and commit hashes}.
Branch: pipeline/{issueKey}

Verify all story commits are on this branch.
Push the branch.
Open a PR using gh pr create:
  Title: {issueKey}: {issue summary}
  Body: Backlog issue {issueKey}, story summaries, acceptance criteria checklist, test output

Then output only this JSON:
{"pr_url":"...","commits":[...]}
```

**Test scope:** unit tests asserting prompt order, presence of all sections, correct phase block per state

---

### GROUP 3 — Story Decomposition

---

### US-006: Claude decomposes issue into user stories

**Priority:** 1  
**Owner:** Claude CLI

**Description:**  
Claude receives the assembled prompt and determines whether decomposition is needed. Small issues → one story. Large issues → ordered stories. Plan is written to file before any code is touched.

**Acceptance Criteria:**
- [ ] Claude explores the codebase itself — OpenFang does not pre-load file lists
- [ ] Plan written to `PIPELINE/PLAN-{issueKey}.md` in story format before JSON output
- [ ] Each story has: title, depends_on, description, acceptance criteria, test scope with exact command
- [ ] Stories ordered by dependency — no story can depend on a later one
- [ ] Single-concern issues: exactly one story — no artificial splitting
- [ ] Stories are independent and testable without running the full test suite
- [ ] Claude outputs JSON matching the schema enforced by `--json-schema` flag

**Test scope:** run decomposition on 3 real Backlog issues, manually review plan quality

---

### US-007: Gate 1 — Human reviews and approves the plan

**Priority:** 1  
**Owner:** OpenFang approval gate

**Description:**  
Pipeline pauses after decomposition. Human reads the plan, optionally edits it, then approves. No code is written until this gate passes.

**Acceptance Criteria:**
- [ ] Terminal shows: issue key + summary, story count, list of story titles
- [ ] Human shown path to plan file: "Review the plan at PIPELINE/PLAN-{key}.md"
- [ ] Options: `[A] Approve` / `[R] Reject — send feedback to Claude` / `[Q] Quit pipeline`
- [ ] On `[A]`: plan file is never modified by Claude again — locked
- [ ] On `[R]`: human types feedback (required), Claude re-decomposes with that feedback appended to the phase instruction
- [ ] Max 2 re-decomposition attempts before escalating to human to edit plan manually
- [ ] Gate waits indefinitely — no timeout

**Test scope:** manual walkthrough on a real issue with each option

---

### GROUP 4 — Story Execution

---

### US-008: Claude executes each story in the same session

**Priority:** 1  
**Owner:** Claude CLI + OpenFang

**Description:**  
Claude implements each story in the same CLI session using `--resume`. It remembers what previous stories built. Each story is committed before the gate. Claude does not proceed to the next story until human approves.

**Acceptance Criteria:**
- [ ] Session ID captured from `response["session_id"]` (top-level field) on first Claude call; stored in `PIPELINE/STATE-{key}.json` alongside `worktree_path`
- [ ] Every subsequent Claude call: `cd {worktree_path}` first, then `--resume {session_id}` (POC-1: sessions are directory-scoped — must always call from worktree root)
- [ ] Schema-enforced output read from `response["structured_output"]` (not `response["result"]`) — validated by POC-2
- [ ] Claude reads `PIPELINE/PLAN-{key}.md` at the start of each story to orient itself
- [ ] Claude implements only the current story — phase instruction explicitly names it
- [ ] Claude runs only the test scope from the plan for this story
- [ ] Claude commits before outputting JSON — commit hash in output
- [ ] `--json-schema` enforces output format — no retry needed for malformed JSON in normal operation
- [ ] **Budget exhaustion detection (POC-2):** `response["subtype"] == "error_max_budget_usd"` AND exit code 1. OpenFang does NOT commit anything and shows a budget-exceeded gate variant:
  ```
  BUDGET EXCEEDED — US-00N incomplete
  Claude hit the $X.XX limit before committing.
  No changes were committed.

  [I] Increase budget and retry   [R] Reject this story   [P] Pause
  ```
- [ ] `[I] Increase budget`: human enters new limit, story retried from scratch (no resume — Claude lost state at budget limit)
- [ ] Budget exhaustion count tracked in state — if it happens twice on same story, pipeline recommends splitting the story manually before retrying

**Test scope:** end-to-end run on a real backend issue — verify session_id reuse across stories

---

### US-009: Targeted test execution only

**Priority:** 1  
**Owner:** Claude CLI

**Description:**  
Claude never runs the full test suite. Test scope is defined per story in the plan and Claude runs exactly that.

**Acceptance Criteria:**
- [ ] Backend: `cargo test -p {crate} {optional_filter}` — derived from files changed
- [ ] Frontend: `curl -s http://127.0.0.1:4200/ | grep -c "{component}"` and JS console check
- [ ] Frontend tests require the OpenFang daemon to be running — the plan must note this and Claude must check with `curl -s http://127.0.0.1:4200/api/health` before testing
- [ ] If daemon is not running for a frontend story: Claude starts it, tests, stops it
- [ ] **Two-tier execution (from SWE-agent research):**
  - *Fast path:* run only tests for the files changed in this story (`cargo test -p {crate} {filter}`)
  - *Validation gate:* if fast path passes, run the crate's full test suite (`cargo test -p {crate}`) before reporting done
  - Fast path failure: fix immediately, do not proceed to validation gate
  - Validation gate failure: fix then re-run both tiers
- [ ] `cargo test --workspace` is explicitly forbidden in the phase instruction
- [ ] Test command and full output included in Claude's JSON response
- [ ] Maximum iterations per story: 10 loops (configurable in `config.toml` as `max_iterations`). If Claude hits the limit without `status: done`, pipeline treats it as `blocked` — same as budget exhaustion path.

**Test scope:** verify no `--workspace` flag appears in any test_run field across 5 issue runs

---

### GROUP 5 — Auto Guards

---

### US-010: Guard runner executes after every story, before gate

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
After Claude's story JSON output is received, OpenFang runs grep-based guards on only the files Claude changed. No LLM. Results shown at the gate.

**Acceptance Criteria:**
- [ ] Guards run only on files listed in Claude's `files_changed` output
- [ ] Guards run even if `test_passed: false` — human sees both
- [ ] Each violation reported as: `{rule_name} — {file}:{line} — {message}`
- [ ] `error` violations listed first, then `warn`
- [ ] Guard run completes in < 5 seconds
- [ ] Guard results stored in `PIPELINE/GUARDS-{key}-US-00N.json`

**Test scope:** unit tests with fixture files containing each baseline violation type

---

### US-011: Guard rules defined per repo in guards.toml

**Priority:** 1  
**Owner:** configuration

**Description:**  
Guard rules are repo-specific and version controlled. Baseline rules always active.

**Baseline rules:**

> **POC-4 result (2026-04-14):** All 8 rules tested against real OpenFang codebase. 5 rules had patterns that produced >80% false positives. Patterns revised below. Rule performance confirmed: 1.08s for full `crates/` scan.

| Rule | Scope | Pattern (revised) | Severity |
|------|-------|---------|----------|
| no_hardcoded_ports | `*.rs` (non-test) | `port.*=.*"\d{4,5}"\|"\d{4,5}".*port\|bind.*"\d{4,5}"` — port context required | error |
| no_hardcoded_ips | `*.rs`, `*.js`, `*.html` | `"\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}"` excluding `0\.0\.0\.0\|127\.\|169\.254\|192\.0\|test\|mock\|block` | error |
| no_unwrap_production | `*.rs` | `\.unwrap\(\)` excluding `#\[test\]\|mod tests\|assert\|let _ =` | **warn** (not error — see note) |
| no_expect_production | `*.rs` | `\.expect\(` | warn |
| no_magic_numbers | `*.rs` (non-test) | skip — POC-4 found 0 matches on real codebase; removed for v1 (revisit if needed) | — |
| business_logic_in_routes | `routes.rs` | Handler function body > 30 lines (awk line-count check — not keyword match) | warn |
| frontend_hardcoded_url | `*.html`, `*.js` | `localhost\|127\.0\.0\.1` | error |
| frontend_invented_route | `*.html`, `*.js` | `fetch\s*\(\s*['"][^/]` | warn |
| no_credentials_in_code | all files | `password\|api_key\|secret.*=.*"` excluding `"test-\|"fake-\|"mock-\|"my-\|"sample-` | error |
| no_todo_left_in_impl | `*.rs` (non-test) | `todo!\(\)\|unimplemented!\(\)` | error |

**Acceptance Criteria:**
- [ ] Baseline rules built into the tool — always run, cannot be removed via config
- [ ] `.pipeline/guards.toml` extends baseline with repo-specific rules
- [ ] Each custom rule requires: name, pattern, files glob, severity, message
- [ ] `exclude` field optional per rule — defaults exclude `*_test.rs` and `*.md`
- [ ] Pattern is a valid regex — tool validates at startup, not at runtime
- [ ] **`no_unwrap_production` note:** Rust puts `#[cfg(test)]` blocks inside the same `.rs` file — filename exclusion alone does not distinguish production code from in-file tests. For v1, this rule is `warn` not `error`. A v2 improvement would use a two-pass check (find `.unwrap()` occurrences, then verify they are not within a `#[cfg(test)]` block by scanning upward for the attribute). Until then, the human gate is the catch for genuine production `unwrap()` calls.

**Test scope:** unit tests for baseline rules on fixture files; test custom rule loading

---

### GROUP 6 — Human Oversight Gates

---

### US-012: Approval gate UI after each story

**Priority:** 1  
**Owner:** OpenFang approval gate

**Description:**  
After each story + guards, terminal shows a structured checkpoint. Human sees guards, diff summary, and test results — then decides.

**Acceptance Criteria:**
- [ ] Gate renders: issue key, story ID + title, cycle count, guard results (coloured), files changed with +/- counts, test command + pass/fail
- [ ] Full `git diff HEAD~1` available inline — gate shows first 40 lines, human can press `[D]` to page through full diff
- [ ] `error`-level guard findings shown in red with file:line
- [ ] `warn`-level findings shown in yellow
- [ ] Options: `[A] Approve` / `[F] Flag` / `[R] Reject` / `[P] Pause`
- [ ] `[A]` with unacknowledged `error`-level guards: requires typing `yes` to confirm override
- [ ] Gate waits indefinitely — no timeout

**Gate format:**
```
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 CHECKPOINT  OFANG-123  US-001: Add budget tracking
 Role: backend · Cycle: 1/3 · Branch: pipeline/OFANG-123
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 GUARDS:
  ✓ no_hardcoded_ports     ✓ no_unwrap_production
  ⚠ magic_numbers — budget.rs:47 — bare 1000, should be config?
  ✓ no_credentials_in_code ✓ no_todo_left_in_impl

 FILES CHANGED:
  + crates/openfang-kernel/src/budget.rs    (+42 -3)
  ~ crates/openfang-kernel/src/agent.rs     (+5 -1)

 TESTS:
  cargo test -p openfang-kernel budget → 3 passed ✓

 COST: $0.43 this issue (US-001: $0.43) · session budget remaining: $0.57

 [D] View full diff
━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━
 [A] Approve   [F] Flag feedback   [R] Reject   [P] Pause
```

**Test scope:** manual walkthrough of all options including diff paging

---

### US-013: Flag feedback — inject into next Claude call

**Priority:** 1  
**Owner:** OpenFang

**Description:**  
`[F] Flag` lets the human send feedback to Claude without rejecting the story. Claude addresses the feedback and amends the commit before the next story begins.

**Acceptance Criteria:**
- [ ] Terminal prompts: "Feedback for Claude (Enter to submit):"
- [ ] Feedback saved to `PIPELINE/FEEDBACK-{key}-US-00N.md`
- [ ] Gap fix phase instruction built with feedback block prepended
- [ ] Claude amends the current story's commit (not a new commit)
- [ ] After fix, guard runner re-runs on updated files
- [ ] Gate shown again with updated results
- [ ] If `[F]` selected 3 times on same story: suggest `[R] Reject` instead with message "Consider rejecting and having Claude restart this story"

**Test scope:** manual test — flag twice, verify feedback appears in Claude's prompt; flag 3 times, verify suggestion

---

### US-014: Reject story — revert and redo

**Priority:** 1  
**Owner:** OpenFang + Claude CLI

**Description:**  
`[R] Reject` reverts the story's commit and Claude redoes the story with the rejection reason. Maximum 3 attempts.

**Acceptance Criteria:**
- [ ] Terminal prompts: "Rejection reason (required):"
- [ ] OpenFang runs `git reset HEAD~1 --mixed` (unstages the story's changes)
- [ ] OpenFang runs `git checkout -- {files_changed}` to restore tracked files to pre-story state
- [ ] OpenFang runs `git clean -fd -- {new_files_from_files_changed}` to remove untracked files created by the story — scoped only to `files_changed`, never the full working tree (avoids nuking unrelated in-progress work)
- [ ] `new_files_from_files_changed` = files in `files_changed` that did not exist before the story (OpenFang cross-references with `git status` output before reset)
- [ ] Claude receives: original story + rejection reason in the execute phase instruction
- [ ] Rejection count tracked per story in `PIPELINE/STATE-{key}.json`
- [ ] After 3 rejections: pipeline pauses, Backlog comment added, human must manually resolve then `pipeline resume`
- [ ] Rejection reason stored in `PIPELINE/FEEDBACK-{key}-US-00N-reject-{cycle}.md`

**Test scope:** manual test — reject once (verify revert + redo); reject 3 times (verify escalation)

---

### GROUP 7 — Session Management

---

### US-015: Pause and resume pipeline

**Priority:** 2  
**Owner:** OpenFang

**Description:**  
At any gate the human can pause. Full state is persisted. `pipeline resume` picks up exactly where it left off using the saved session ID.

**Acceptance Criteria:**
- [ ] `[P] Pause` writes final state to `PIPELINE/STATE-{key}.json`
- [ ] State includes: issue key, role, branch, `worktree_path`, session_id, phase, current story ID, completed story IDs + commit hashes, cycle counts, session_note
- [ ] `pipeline resume {issueKey}` reads state, `cd {worktree_path}`, resumes from last incomplete story
- [ ] Resume uses `--resume {session_id}` with cwd = `{worktree_path}` (POC-1: directory-scoped)
- [ ] **Expired/invalid session detection (POC-1):** exit code 1 + stderr `"No conversation found with session ID: {id}"`. On detection: start fresh session (no `--resume`), prepend all accumulated handoff notes to prompt
- [ ] Backlog comment added on pause and on resume

**Test scope:** manual test — pause mid-issue, terminate process, resume, verify continuation

---

### US-016: Session continuity — handoff_note after every story

**Priority:** 1 ← upgraded from 2  
**Owner:** OpenFang + Claude CLI

**Description:**  
`--resume {session_id}` is the primary continuity mechanism but can fail (expired session, crash between stories, budget exhaustion). The `handoff_note` field in every execute-phase JSON response is the fallback — it is always written, always saved, costs almost nothing, and means continuity is never fully lost.

**Acceptance Criteria:**
- [ ] Every execute-phase JSON schema **requires** `handoff_note` — Claude must produce it or the schema validation fails
- [ ] `handoff_note` content: one paragraph — "Completed X. Changed files A, B, C. Key decisions: D. Next story should know: E."
- [ ] OpenFang saves `handoff_note` to `PIPELINE/HANDOFF-{key}-US-00N.md` after every approved story
- [ ] **On resume with working session_id:** `--resume` used, no handoff needed — Claude already has context
- [ ] **On resume with expired/failed session_id:** new session started, prompt includes all `handoff_note` files from completed stories concatenated — Claude re-orients from these before continuing
- [ ] **Session length threshold:** `max_stories_per_session` configurable in `config.toml` (default: 5). At threshold, proactively start a new session using accumulated `handoff_note` files — do not wait for session to fail
- [ ] Token count estimated before each story. If estimated > 70k tokens, trigger new session regardless of story count
- [ ] Human notified on proactive handoff: "Starting fresh session for US-006 onwards (session length limit reached)"
- [ ] Every execute-phase JSON schema requires `progress_update` — structured patterns discovered this story (codebase gotchas, file dependencies, test quirks, unexpected behaviour)
- [ ] OpenFang appends each story's `progress_update` to `PIPELINE/PROGRESS.md` after approval — one section per story, newest last
- [ ] **`PIPELINE/PROGRESS.md` is mandatory in every prompt** (POC-11: 38% cost reduction, 145 tokens/story overhead confirmed). Prompt order: CLAUDE.md → PROGRESS.md → role file → phase instruction → issue. Always included, not conditional.
- [ ] If `PIPELINE/PROGRESS.md` exceeds 2k tokens (after ~14 stories): truncate to most recent entries that fit within 2k.
- [ ] Validate `max_stories_per_session` is between 1 and 20. If out of range: use default 5 and log warning.

**Test scope:** manual test with 7-story issue and `max_stories_per_session = 3`

---

### GROUP 8 — Completion

---

### US-017: PR creation targeting agent/dev

**Priority:** 1  
**Owner:** Claude CLI + OpenFang

**Description:**  
After all stories are approved, Claude pushes `pipeline/{issueKey}` and opens a PR targeting `agent/dev` — not the repo's base branch. Merging to the repo base branch (`dev` or `main`) is a separate human-driven action on `agent/dev` when ready to ship.

**Acceptance Criteria:**
- [ ] Claude verifies all expected story commits are on `pipeline/{issueKey}` before pushing
- [ ] Claude explicitly verifies it is NOT on `{base_branch}` (or `main`) before pushing — pipeline aborts with error if so
- [ ] Claude pushes: `git push -u origin pipeline/{issueKey}`
- [ ] **Draft PR timing:** after Gate 1 (plan approved), Claude opens a draft PR immediately — visible early without blocking review queue (Sweep.dev pattern): `gh pr create --draft --base agent/dev --head pipeline/{issueKey}`
- [ ] **Idempotency:** before creating PR, check if one already exists: `gh pr list --head pipeline/{issueKey} --base agent/dev`. If found: reuse that PR URL instead of creating a duplicate.
- [ ] After all stories approved at final gate: Claude converts draft to ready: `gh pr ready {pr_number}`
- [ ] PR title: `{issueKey}: {issue summary}`
- [ ] PR body template:
  ```
  **Backlog:** {issueKey} | **Role:** {role}

  ## Stories
  - {US-001}: {title} — {short_commit_hash}
  - {US-002}: {title} — {short_commit_hash}

  ## Acceptance Criteria
  {checklist from plan file}

  ## Tests
  {last test output summary}
  ```
- [ ] PR URL returned in Claude's JSON output, stored in `PIPELINE/STATE-{key}.json`
- [ ] OpenFang confirms PR targets `agent/dev` from `gh pr view` output before reporting success
- [ ] After PR is opened, OpenFang marks Backlog issue "In Review" and adds PR URL as comment

**Periodic agent/dev → base branch release (out of pipeline scope):**  
This is a manual human action — not automated. Human opens PR `agent/dev → dev` (or `main`) when ready to ship a batch. The pipeline's job ends at `agent/dev`.

**Test scope:** end-to-end run — verify PR base is `agent/dev`, body correct, commits clean; verify error on wrong branch

---

### US-018: Repo setup command

**Priority:** 1 ← prerequisite — must run before any issue enters the pipeline  
**Owner:** CLI command + Claude CLI

**Description:**  
One-time command to bootstrap `.pipeline/` for a new repo. Claude reads the codebase to generate role-specific quality standards that actually match the repo's conventions.

**Acceptance Criteria:**
- [ ] `pipeline setup` checks for existing `CLAUDE.md` — warns if missing (does not create it); hard-stops if `CLAUDE.md` is absent and `--force` not passed
- [ ] Creates `.pipeline/config.toml` with all required fields as commented placeholders:
  ```toml
  base_branch = "dev"          # Change to "main" if this repo works off main
  backlog_base = "TODO"        # e.g. https://yourorg.backlog.com
  backlog_project = "TODO"     # e.g. MYPROJECT
  max_budget_usd = 3.00        # Per-story Claude CLI cost cap
  max_stories_per_session = 5  # Force session refresh after N stories
  max_iterations = 10          # Max retry loops per story before blocking
  max_cycles_per_story = 3     # Max Flag/Reject cycles before escalation
  repo_map_lines = 0           # 0 = disabled (default). Set to 100 for large unfamiliar repos.
  ```
- [ ] Creates `.pipeline/guards.toml` with all 9 baseline rules pre-populated and enabled
- [ ] Claude reads repo structure and writes `.pipeline/backend.md` and `.pipeline/frontend.md` that reflect actual conventions (not generic templates)
- [ ] Idempotent: re-running does not overwrite files that have been customised (checks `git status` for each file)
- [ ] `pipeline setup --refresh` regenerates role files from current codebase — useful when architecture changes

**Test scope:** run on openfang repo, manually review generated files for accuracy

---

### US-019: Pipeline dashboard tab (Tauri + web)

**Priority:** 2  
**Owner:** OpenFang fork — `crates/openfang-api/static/index_body.html`

**Description:**  
Add a **Pipeline** tab to the existing OpenFang Alpine.js dashboard. Because OpenFang ships as a Tauri 2.0 native desktop app (webview over the same `index_body.html`), this tab appears in both the web UI and the macOS/Windows desktop app automatically — no Tauri-specific code needed.

**What it shows:**

```
┌─ Pipeline ──────────────────────────────────────────────────────┐
│  Active issue:  OFANG-142  "Add webhook retry logic"            │
│  Branch:        pipeline/OFANG-142   Cost so far: $0.23         │
│                                                                  │
│  Stories:  US-001 ✓  US-002 ✓  US-003 ⟳  US-004 ○  US-005 ○  │
│                                                                  │
│  Current story: US-003 — Implement retry backoff                │
│  Phase: EXECUTE  [running 2m 14s]                                │
│  Guards: ✓ no_hardcoded_secrets  ✓ no_todos  ✗ no_unwrap (WARN) │
│  Tests:  cargo test -p openfang-kernel webhook — PASS            │
│                                                                  │
│  ──── Last 10 completed issues ────────────────────────────────  │
│  OFANG-139  PR #88  $0.41  ✓ merged                             │
│  OFANG-137  PR #85  $0.29  ✓ merged                             │
│  OFANG-133  PR #82  $0.18  ⏳ awaiting review                   │
└─────────────────────────────────────────────────────────────────┘
```

**Data source:**  
OpenFang exposes pipeline state via a new endpoint `GET /api/pipeline/status` that the Alpine.js tab polls every 3 seconds. The pipeline binary writes state to OpenFang's memory substrate; the API route reads it.

**Acceptance Criteria:**
- [ ] New "Pipeline" tab appears in the dashboard tab bar between "Workflows" and "Scheduler"
- [ ] Active issue panel: issue key, branch, elapsed time, running cost
- [ ] Story progress bar: one badge per story — `✓` done, `⟳` in progress, `○` pending, `✗` failed
- [ ] Guard results for current story: pass/warn/fail per rule with rule name
- [ ] Test result line: command + pass/fail status
- [ ] Last 10 completed issues table: issue key, PR link, total cost, merge status
- [ ] Tab shows "No pipeline running" state when idle (not an error state)
- [ ] `GET /api/pipeline/status` endpoint registered in `server.rs` and wired to pipeline state in memory substrate
- [ ] Works in both browser (`http://127.0.0.1:4200`) and Tauri desktop app without modification
- [ ] Human approval gate: when a story reaches the GATE phase, the Pipeline tab highlights the story badge in amber and shows an inline [Approve] / [Flag] / [Reject] row — clicking calls `POST /api/pipeline/gate/{storyId}/{decision}`

**Test scope:** start a real pipeline run against openfang repo; confirm tab updates in real-time in both web browser and Tauri app

---

### US-020: Deployment modes — local, server daemon, SSH

**Priority:** 2  
**Owner:** `pipeline` CLI binary

**Description:**  
The pipeline must run in three modes with equal correctness:

| Mode | Setup | Monitoring | Best for |
|------|-------|-----------|---------|
| **Local interactive** | `pipeline run OFANG-123` in terminal | Terminal output + Tauri dashboard tab | Daily dev work on local machine |
| **Local daemon** | `pipeline start --daemon` | Logs in `~/.pipeline/logs/`, Tauri dashboard tab | Long-running overnight runs |
| **Remote server** | `pipeline start --daemon` on Ubuntu via SSH | SSH into server, `pipeline logs`, tail log file | Team server, CI-adjacent use |

**Local machine use cases powered by the pipeline + Tauri UI:**
- Watch active story execution in real time in the dashboard
- One-click approve/flag/reject via the Pipeline tab (US-019)
- Browse full execution history, cost breakdown, guard results
- Compare diff of any story from the Last 10 issues table
- `pipeline doctor` in terminal to check all auth and config before running

**Server deployment flow:**
```bash
# On Ubuntu server (one-time setup)
ssh user@your-server
git clone https://github.com/your-org/openfang
cargo build --release -p pipeline
export ANTHROPIC_API_KEY=...
export BACKLOG_API_KEY=...

# Start as daemon
pipeline start --daemon --repo /path/to/target/repo

# From local machine: SSH to check logs
ssh user@your-server tail -f ~/.pipeline/logs/pipeline.log

# Or use pipeline CLI to query state remotely
pipeline status --remote user@your-server
```

**Acceptance Criteria:**
- [ ] `pipeline run {issueKey}` — interactive mode: stdout + stderr to terminal, SIGINT/Ctrl-C triggers [P] Pause gracefully (writes STATE file before exit)
- [ ] `pipeline start --daemon` — detaches from terminal, writes PID to `~/.pipeline/pipeline.pid`, logs to `~/.pipeline/logs/pipeline-{date}.log`
- [ ] `pipeline stop` — reads PID file, sends SIGTERM, waits for graceful shutdown (STATE file written), removes PID file
- [ ] `pipeline logs [--follow]` — tails the current log file; `--follow` equivalent to `tail -f`
- [ ] `pipeline status` — reads `PIPELINE/STATE-{key}.json` and prints current phase, story, elapsed time, running cost; works without daemon running (reads file directly)
- [ ] `pipeline status --remote user@host` — SSHes to host, runs `pipeline status`, prints result locally
- [ ] Daemon writes structured log lines: `{timestamp} [{level}] [{issueKey}] {message}` — parseable by standard log tools
- [ ] Log rotation: new log file per day, keep last 30 days
- [ ] On server: dashboard tab available at `http://{server-ip}:4200` if OpenFang daemon is also running — same Alpine.js UI, accessible from local browser over LAN/VPN
- [ ] `pipeline abandon {issueKey}` — hard-stops any in-progress run for that issue, resets STATE file, resets Backlog status to "Open"

**Shell access (headless operation):**
- All gates have a terminal readline fallback when OpenFang dashboard is not available: print gate prompt, read `[A/F/R/P]` from stdin
- When running via SSH, gates wait for stdin input over the SSH session
- `--non-interactive` flag: auto-approve all gates (for fully automated dry-run testing only — not for production)

**Test scope:** test daemon start/stop/logs on macOS; deploy to Ubuntu 24.04 LTS server, run one full issue, verify logs and remote status

---

## Open Questions

| # | Question | Blocks |
|---|----------|--------|
| 7 | `--max-budget-usd` default: $1.00 is set — validate against real multi-story runs in Phase 3 | US-005 |
| 8 | Claude session retention period — how long before `--resume {id}` stops working? Validate empirically during Phase 4. | US-015/016 |

**Resolved questions:**
- ~~PR target: gh vs Gitea~~ → **GitHub CLI (`gh`)** used for PR creation. Gitea support deferred to v2.
- ~~Approval gate: readline vs full TUI~~ → **Dashboard tab** (US-019) handles gate approval via web UI. Terminal fallback: simple readline for headless/SSH mode.
- ~~pipeline setup and CLAUDE.md~~ → **Require pre-existing** — pipeline hard-stops if missing (US-018 updated).
- ~~Language / standalone vs crate~~ → **Rust, standalone binary** (`pipeline` CLI). Standalone because the pipeline has its own release cycle and must work against any repo, not just OpenFang.
- ~~Claude CLI flags~~ → confirmed: `-p`, `--dangerously-skip-permissions`, `--output-format json`, `--json-schema`, `--resume`, `--max-budget-usd`
- ~~Branch strategy~~ → `agent/dev` integration branch, `pipeline/{key}` per-issue, PRs target `agent/dev`
- ~~Base branch~~ → configurable per repo (`base_branch = "dev"` default) — most repos work off `dev` first
- ~~Fullstack issues~~ → rejected at classification in v1 with Backlog comment instructing split
- ~~JSON output enforcement~~ → `--json-schema` on every call; `handoff_note` field required in every execute response
- ~~Backlog webhook vs polling~~ → **Polling** for v1 (no public URL needed). Webhook upgrade deferred to v2. (POC-3: project had no webhooks configured; polling confirmed working)
- ~~Backlog dependency field~~ → **`parentIssueId`** confirmed by POC-3. No `relatedIssues` field exists. US-003b updated.

---

## Risks

| Risk | Likelihood | Mitigation | Covered by |
|------|-----------|------------|-----------|
| Claude implements beyond story scope | Medium | Phase instruction explicitly names only current story; human gate catches it | US-007, US-012 |
| Guard false positives (e.g. port pattern matching comments) | Medium | Patterns scoped to string/const context not free text; tune during Phase 3 | US-011, POC-4 |
| Session expired on resume | Medium | Detect expired session, auto-start new with handoff summary | US-015, US-016, POC-1 |
| `CLAUDE.md` is stale or missing | High | US-018 setup command; US-005 hard-stops if CLAUDE.md missing | US-005, US-018 |
| `gh` auth fails in pipeline context | Low | `pipeline doctor` checks `gh auth status` on startup | US-000 |
| Frontend tests require running daemon | Medium | Phase instruction tells Claude to check + start daemon | US-009 |
| Pipeline crashes mid-story, state lost | Medium | `PIPELINE/STATE-{key}.json` written after every step; `pipeline resume` on restart | US-015 |
| Cost overrun per issue | Medium | Per-story `--max-budget-usd` cap; configurable in `config.toml`; shown at every gate | US-008, US-019 |

---

## Out of Scope (v1)

| Item | Why deferred | Workaround |
|------|-------------|-----------|
| Parallel issue execution | Adds complexity and debugging difficulty; reliability first | Run multiple daemon instances on separate repo clones |
| Auto-merge on PR approval | Human review always required | Human merges manually after pipeline opens PR |
| CI/CD integration (watching CI results) | Out of pipeline's domain | Human monitors CI; pipeline's job ends at `agent/dev` PR |
| Full terminal TUI (Ratatui/Bubble Tea) | Dashboard tab (US-019) + readline fallback covers all cases | Use Tauri app or `pipeline logs --follow` |
| Support for repos without `CLAUDE.md` | Too risky — Claude needs repo context | Run `pipeline setup` first; it warns if missing |
| Multi-repo issues | Same issue touching backend + frontend repos | Split into two issues in Backlog; link as dependencies |
| Gitea PR support | `gh` CLI is GitHub-only | Use GitHub; Gitea deferred to v2 |
| Rollback of merged PRs | Manual git operation outside pipeline scope | `git revert` or `git reset` manually on `agent/dev` |
