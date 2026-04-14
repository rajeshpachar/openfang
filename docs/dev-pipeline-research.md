# Dev Pipeline — Research Findings
**Status:** Reference document — do not edit without adding a dated note  
**Linked from:** `docs/dev-pipeline-prd.md`  
**Last updated:** 2026-04-14  

---

## Section 1 — OpenFang Existing Capabilities

> Explored by reading all 14 crates in `/Users/rajesh/Documents/GitHub/openfang/crates/`.  
> Full capability inventory so the pipeline does not rebuild what already exists.

### 1.1 Workflow Engine (use as pipeline state machine)

**Location:** `crates/openfang-kernel/src/workflow.rs`

OpenFang already has a multi-step workflow engine with:

```rust
WorkflowStep {
    name, agent, prompt_template,
    mode: Sequential | FanOut | Collect | Conditional{condition} | Loop{max_iterations},
    error_mode: Fail | Skip | Retry{max_retries},
    output_var,   // pass output of step N into step N+1
    timeout_secs
}
```

**Implication for pipeline:** The decompose → implement → guard → gate → commit sequence maps directly onto a `Sequential` workflow. The gap fix loop maps onto `Loop{until: "passed == true"}`. **We should not build a custom state machine from scratch — wire pipeline phases as workflow steps instead.**

Dashboard already has a `/workflows` tab with CRUD + execution UI.

---

### 1.2 Task Queue (use for story work distribution)

**Location:** `crates/openfang-memory/src/substrate.rs` (lines 422–541)

SQL-backed task queue with:
- `task_post(title, description, assigned_to, created_by)` → `task_id`
- `task_claim(agent_id)` → claims next pending task (priority + FIFO)
- `task_complete(task_id, result)`
- `task_list(status)` → filter by pending/in_progress/completed

**Implication:** Each user story can be a task in this queue. The pipeline posts all stories from decomposition, then claims them one at a time. Resume = re-claim the in_progress task on restart. This replaces the custom `PIPELINE/STATE-{key}.json` file for story tracking.

---

### 1.3 Event Bus + Triggers (use for stage coordination)

**Location:** `crates/openfang-kernel/src/event_bus.rs` and `triggers.rs`

- Broadcast channel (1024 capacity), per-agent channels (256), history ring buffer (1000 events)
- Agents subscribe via `TriggerPattern` — e.g., `ContentMatch{"story_complete"}` or `MemoryKeyPattern{"pipeline/*"}`
- `Trigger.prompt_template` uses `{{event}}` substitution

**Implication:** Each pipeline phase can publish an event on completion. Gate approval publishes `gate_approved`. The next phase is triggered automatically. This is cleaner than polling a state file.

---

### 1.4 Budget and Metering (use for per-story cost caps)

**Location:** `crates/openfang-kernel/src/metering.rs`

- Per-agent hourly/daily/monthly quotas
- `check_quota(agent_id, quota)` — hard stop before LLM call if quota exceeded
- `estimate_cost(model, tokens)` — pre-flight estimate
- `get_summary(agent_id)` — cumulative spend

**Implication:** Each pipeline run is an agent. `--max-budget-usd` on the Claude CLI call is the per-story cap. OpenFang's metering tracks the cumulative issue cost and can stop the pipeline if a configurable budget is reached.

---

### 1.5 Approval System (use for human gates)

**Location:** `crates/openfang-kernel/src/approvals.rs` (referenced in `kernel_handle.rs`)

```rust
requires_approval(tool_name) -> bool
request_approval(agent_id, tool_name, summary) -> bool
```

Dashboard shows a **pending approval badge counter** on the Agents tab. Approvals block the agent loop until human responds.

**Implication:** Human oversight gates (US-012 in PRD) can be implemented using this existing approval system instead of building a custom TUI. The pipeline submits a "story complete — approve to continue?" approval request. Human sees it in the dashboard (or terminal). No new UI needed.

---

### 1.6 Webhook System (use for Backlog → pipeline trigger)

**Location:** `crates/openfang-types/src/webhook.rs`

```
POST /hooks/wake  { text, mode: Now | NextHeartbeat }
POST /hooks/agent { message, agent?, deliver?, channel?, model?, timeout_secs }
```

**Implication:** Instead of polling Backlog every 5 minutes, register a Backlog webhook that POSTs to `/hooks/agent` when an issue is moved to "Ready for Pipeline" status. Eliminates polling entirely.

---

### 1.7 Dashboard (where pipeline visibility goes)

**Location:** `crates/openfang-api/static/index_body.html`

Existing tabs: Chat, Overview, Analytics, Logs, Sessions, Approvals, Comms, Workflows, Scheduler, Channels, Skills, Hands, Runtime, Settings.

**Implication:** Add a **"Pipeline"** tab alongside "Workflows". Shows:
- Active issue key + current story
- Story progress bar (US-001 ✓, US-002 ✓, US-003 ⟳)
- Guard results for current story
- Running cost this issue
- Last 10 completed issues with PR links

This is an Alpine.js addition to the existing dashboard — no new server needed.

---

### 1.8 A2A Protocol (future: external integrations)

**Location:** `crates/openfang-runtime/src/a2a.rs`

Google's cross-framework agent interop standard. OpenFang agents expose capabilities as A2A skills. External systems can submit tasks via REST.

**Implication:** In a future version, a Slack bot or GitHub Actions workflow could submit issues to the pipeline via A2A. Out of scope for v1 but worth knowing the hook is there.

---

### 1.9 Comms Topology (pipeline agent visibility)

**Location:** `crates/openfang-types/src/comms.rs`

`GET /api/comms/topology` returns a graph of agent relationships. Dashboard "Comms" tab visualises this.

**Implication:** The pipeline orchestrator agent, when registered with OpenFang, appears in this graph automatically. No extra work needed for basic visibility.

---

### 1.10 Process Manager (Claude CLI subprocess)

**Location:** `crates/openfang-runtime/src/process_manager.rs`

Manages long-running processes — REPLs, servers, persistent sessions.

**Implication:** Claude CLI can be managed as a persistent process via this manager rather than spawning a new subprocess per call. Sessions that need `--resume` benefit from the process staying warm.

---

## Section 2 — Tool Comparison

> Research across Chief, SWE-agent, Sweep.dev, Aider, and OpenHands.  
> Sources listed in Section 4. Only actionable learnings recorded here.

---

### 2.1 Chief (most directly relevant)

**What it does:** Breaks projects into PRD stories, runs Claude CLI in a loop, one commit per story.

**Key patterns:**

| Pattern | Description | Our application |
|---------|-------------|-----------------|
| **Ralph Wiggum Loop** | Fresh context per story, progress persisted in `progress.md` | Matches our `handoff_note` per story — strengthen to full `progress.md` |
| **Worktrees per PRD** | Each PRD runs in isolated git worktree — no file conflicts | Use per-story worktrees (stronger than per-issue branches) |
| **One commit per story** | Clean git history, easy bisect | Already in our design |
| **`chief review`** | Separate audit pass comparing implementation vs PRD acceptance criteria | Maps to our Review phase |
| **`chief resume`** | Resume completed story's session | Maps to our `--resume {session_id}` |
| **Chief backlog command** | Already has Backlog (Nulab) integration | Study the implementation — may reuse |
| **Structured completion signal** | Agent outputs `<chief-done/>` not just "I'm done" | We use `--json-schema` enforcement instead — better |
| **Max iterations guard** | Per-story iteration cap prevents infinite loops | We have `--max-budget-usd` — add iteration cap too |

**What chief does NOT have that we add:**
- Approval gates with feedback injection
- Auto guards (grep-based pattern detection)
- Per-story cost display
- Backlog status updates (comments, status changes)
- `agent/dev` integration branch model
- Chained issue dependency detection

---

### 2.2 SWE-agent

**What it does:** Standalone AI software engineer, targets SWE-bench challenges.

**Key patterns:**

| Pattern | Description | Our application |
|---------|-------------|-----------------|
| **Two-tier testing** | Targeted tests first → full suite only if targeted pass | Add to US-009: fast path + validation gate |
| **Structured failure diagnosis** | `failure_type: test_failure \| compilation_error \| timeout \| api_error` | Add structured failure metadata to execute phase output |
| **Configuration-driven prompts** | Full agent behaviour in YAML | We avoid this — prompts embedded in code (simpler) |

**What NOT to copy:** SWE-agent maintains full conversation history — proven to hit context limits on complex tasks. Our `handoff_note` + fresh sessions is better.

---

### 2.3 Aider

**What it does:** AI pair programmer, specialises in multi-file edits.

**Key patterns:**

| Pattern | Description | Our application |
|---------|-------------|-----------------|
| **Repository map** | Function/class signatures + call graph ranked by PageRank relevance | Add repo map generation to `pipeline setup` — inject top 1–2k tokens into every prompt |
| **Function-level dependency tracking** | "If you edit F, you must also update G (callers/callees)" | Repo map captures this — Claude sees the connections |

**Repository map detail:** Aider runs `ctags` or `tree-sitter` to extract symbol definitions, builds a graph, ranks by PageRank (files touched by similar previous tasks score higher), sends only top N tokens. Token cost: ~1–2k. Value: Claude navigates multi-file changes accurately without needing full file contents.

---

### 2.4 Sweep.dev

**What it does:** GitHub-native AI that turns issues into PRs.

**Key patterns:**

| Pattern | Description | Our application |
|---------|-------------|-----------------|
| **Draft PR first** | Opens `--draft` PR immediately, converts to ready when complete | Add: `gh pr create --draft` at story start, convert on approval |
| **Always human approval before merge** | Never auto-merges — human PR review required | Already in our design |
| **Issue → PR in one flow** | Reads issue, writes code, opens PR with issue linked | Our pipeline does this |

---

### 2.5 OpenHands (formerly OpenDevin)

**What it does:** Full software development agent with Docker sandbox.

**Key patterns:**

| Pattern | Description | Our application |
|---------|-------------|-----------------|
| **Hierarchical agents** | Orchestrator spawns sub-agents for parallel subtasks | Future: use OpenFang's `spawn_agent` + `task_post` for parallel stories |
| **Soft vs hard gates** | Destructive actions (delete, push) need approval; read/write do not | Our model: commit is soft (no gate), push+PR is hard (after all stories approved) |
| **Disposable sandboxes** | Docker container per session — teardown on complete | We use git worktrees instead — same isolation, no Docker dependency |
| **Interrupt on approval** | LangGraph `interrupt()` mechanism | We use OpenFang's existing approval system |

---

## Section 3 — Design Decisions Derived From Research

These decisions update or extend the PRD based on research findings:

| Decision | Source | PRD impact |
|----------|--------|------------|
| Use OpenFang Workflow Engine as state machine | OpenFang codebase | Replace custom state machine with workflow steps |
| Use OpenFang Task Queue for story tracking | OpenFang codebase | Replace `PIPELINE/STATE-{key}.json` story list with task queue |
| Use OpenFang Approval system for gates | OpenFang codebase | Gate UI renders in dashboard, not just terminal |
| Use Backlog webhook → `/hooks/agent` instead of polling | OpenFang codebase | Eliminates polling; real-time trigger |
| Add repo map (1–2k tokens) to every prompt | Aider | Reduces hallucination about file structure |
| Two-tier testing: targeted → full suite | SWE-agent | Update US-009 test strategy |
| Structured failure diagnosis in execute output | SWE-agent | Add `failure_type` field to execute JSON schema |
| `gh pr create --draft` at pipeline start, convert on completion | Sweep.dev | Update US-017 |
| `progress.md` pattern — accumulate patterns, not history | Chief | Strengthen `handoff_note` to structured `progress.md` |
| Add iteration cap per story alongside budget cap | Chief | Update US-008 |
| Per-story worktrees (not per-issue branches) | Chief | Consider upgrade from per-issue branches |

---

## Section 4 — References

| Source | URL | Key insight used |
|--------|-----|------------------|
| Chief README | https://minicodemonkey.github.io/chief/ | Ralph loop, worktrees, one-commit-per-story |
| Chief — How It Works | https://minicodemonkey.github.io/chief/concepts/how-it-works | Progress.md pattern, session resume |
| Chief — Ralph Loop concept | https://minicodemonkey.github.io/chief/concepts/ralph-loop | Fresh context per iteration |
| Geoffrey Huntley — Ralph pattern | https://ghuntley.com/ralph/ | Original Ralph Wiggum loop pattern |
| SWE-agent NeurIPS 2024 | https://proceedings.neurips.cc/paper_files/paper/2024/file/5a7c947568c1b1328ccc5230172e1e7c-Paper-Conference.pdf | Failure taxonomy, context overflow patterns |
| Why Agentic PRs Get Rejected | https://arxiv.org/html/2602.04226v1 | 67.9% feedback-less rejections; oversized submissions |
| Aider — Repository Map | https://aider.chat/docs/repomap.html | PageRank repo map, symbol extraction |
| Sweep.dev deep dive | https://skywork.ai/skypage/en/sweep-ai-development-guide/1976898964182593536 | Draft PR pattern, issue→PR flow |
| OpenHands SDK | https://arxiv.org/html/2511.03690v1 | Hierarchical agents, soft vs hard gates |
| OpenHands Docker sandbox | https://docs.openhands.dev/sdk/guides/agent-server/docker-sandbox | Disposable sandboxes vs worktrees |
| OpenFang codebase | /Users/rajesh/Documents/GitHub/openfang/crates/ | All OpenFang capability findings (Section 1) |
| Backlog API docs | https://developer.nulab.com/docs/backlog/ | Issue fields, webhook support, dependency fields |
