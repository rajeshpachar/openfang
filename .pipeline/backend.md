# Backend Standards

```markdown
# Backend Standards

## Language & Architecture

Rust (2021 edition) across 15 workspace crates under `crates/`. Key patterns:
- `openfang-types` owns all shared domain types; other crates depend on it, not each other
- `openfang-kernel` is the core runtime; `openfang-api` bridges it to HTTP via `AppState`
- `openfang-pipeline` is a standalone CLI tool using `clap` with subcommands
- Async everywhere via `tokio` with `features = ["full"]`; traits with async methods use `async-trait`

## File Organization

- One concept per file, snake_case names (`event_bus.rs`, `runner.rs`, `config.rs`)
- No `mod.rs` — declare submodules in the parent (e.g. `lib.rs`: `pub mod doctor;`)
- Group related files in subdirectories only when there are 3+ siblings (`commands/`, `drivers/`)
- New types → `openfang-types/src/`; new API routes → `openfang-api/src/routes.rs` + registered in `server.rs`
- Config fields require: struct field + `#[serde(default)]` + entry in `Default` impl

## Running Tests

```bash
cargo build --workspace --lib          # compile check (use --lib if .exe is locked)
cargo test --workspace                 # all 1744+ unit + integration tests
cargo clippy --workspace --all-targets -- -D warnings  # zero warnings required
```

LLM integration tests skip automatically when `GROQ_API_KEY` is unset.

## Error Handling

Split by call site:
- **Library code** (`openfang-types`, `openfang-kernel`, `openfang-runtime`): `thiserror` with named enum variants and descriptive `#[error("...")]` messages. Use type aliases: `pub type KernelResult<T> = Result<T, KernelError>;`
- **Operational/CLI code** (`openfang-pipeline`, `openfang-api` handlers): `anyhow::{bail, Context, Result}` with `.context("Failed to ...")` at every fallible call

Never use `unwrap()` or `expect()` in non-test code.

## Logging

`tracing` exclusively — no `log` crate. Import only what you use:
```rust
use tracing::{debug, info, warn, error};
```
Terminal output in pipeline commands uses `colored` (`.green()`, `.red()`) with `✓ ✗ ⚠` status symbols.

## Key Pitfalls

- Routes added to `routes.rs` **must** also be registered in the `server.rs` router or they are dead code
- `KernelConfig` struct fields must appear in both the struct definition **and** the `Default` impl
- `AgentLoopResult` response field is `.response`, not `.response_text`
- `PeerRegistry` wrapping: kernel holds `Option<PeerRegistry>`, `AppState` holds `Option<Arc<PeerRegistry>>`
- Line width limit is **100 chars** (`rustfmt.toml`), not the default 80

## Push Contract

All three must pass before committing:
1. `cargo build --workspace --lib` — zero errors
2. `cargo test --workspace` — zero failures
3. `cargo clippy --workspace --all-targets -- -D warnings` — zero warnings

After any new endpoint or wiring change, run a live integration test against the running daemon (see `CLAUDE.md` for full procedure).
```