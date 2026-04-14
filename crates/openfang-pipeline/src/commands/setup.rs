/// US-018: `pipeline setup` -- bootstrap `.pipeline/` for a new repo.
use anyhow::{bail, Context, Result};
use colored::Colorize;
use std::path::Path;
use std::process::Command;

use crate::config::{config_toml_template, guards_toml_template};

pub struct SetupArgs {
    pub force: bool,
    pub refresh: bool,
    pub repo_root: std::path::PathBuf,
}

pub fn run(args: SetupArgs) -> Result<()> {
    let repo_root = &args.repo_root;

    println!("\n{}", "Pipeline Setup".bold());
    println!("{}", "-".repeat(50));

    // 1. Check CLAUDE.md
    check_claude_md(repo_root, args.force)?;

    // 2. Create .pipeline/ directory
    let pipeline_dir = repo_root.join(".pipeline");
    std::fs::create_dir_all(&pipeline_dir)
        .with_context(|| format!("Failed to create {}", pipeline_dir.display()))?;

    // 3. config.toml
    create_if_absent(
        &pipeline_dir.join("config.toml"),
        &config_toml_template(),
        "config.toml",
        args.refresh,
    )?;

    // 4. guards.toml
    create_if_absent(
        &pipeline_dir.join("guards.toml"),
        &guards_toml_template(),
        "guards.toml",
        args.refresh,
    )?;

    // 5. backend.md and frontend.md
    let backend_path = pipeline_dir.join("backend.md");
    let frontend_path = pipeline_dir.join("frontend.md");

    if args.refresh || !backend_path.exists() || !frontend_path.exists() {
        generate_role_files(repo_root, &pipeline_dir, args.refresh)?;
    } else {
        println!(
            "  {} .pipeline/backend.md   already exists (use --refresh to regenerate)",
            "-".dimmed()
        );
        println!(
            "  {} .pipeline/frontend.md  already exists (use --refresh to regenerate)",
            "-".dimmed()
        );
    }

    // 6. Create PIPELINE/ directory for runtime state
    let pipeline_state_dir = repo_root.join("PIPELINE");
    std::fs::create_dir_all(&pipeline_state_dir)
        .with_context(|| format!("Failed to create {}", pipeline_state_dir.display()))?;

    // Add PIPELINE/ to .gitignore
    ensure_gitignore(repo_root)?;

    println!("{}", "-".repeat(50));
    println!("  {} Setup complete\n", "OK".green().bold());
    println!("  Next steps:");
    println!("    1. Edit .pipeline/config.toml -- fill in backlog_base and backlog_project");
    println!("    2. Set BACKLOG_API_KEY environment variable");
    println!("    3. Run: pipeline doctor");
    println!("    4. Run: pipeline run <ISSUE-KEY>\n");

    Ok(())
}

fn check_claude_md(repo_root: &Path, force: bool) -> Result<()> {
    let claude_md = repo_root.join("CLAUDE.md");
    if !claude_md.exists() {
        if force {
            println!(
                "  {} CLAUDE.md not found -- continuing anyway (--force)",
                "WARN".yellow()
            );
        } else {
            bail!(
                "No CLAUDE.md found at {}.\n\nCLAUDE.md contains repo conventions that guide Claude.\nCreate one, then re-run `pipeline setup`.\nUse --force to skip this check.",
                claude_md.display()
            );
        }
    } else {
        let size = std::fs::metadata(&claude_md).map(|m| m.len()).unwrap_or(0);
        println!("  {} CLAUDE.md found ({} bytes)", "OK".green(), size);
    }
    Ok(())
}

fn create_if_absent(path: &Path, content: &str, label: &str, refresh: bool) -> Result<()> {
    if path.exists() && !refresh {
        println!("  {} .pipeline/{}   already exists", "-".dimmed(), label);
        return Ok(());
    }
    std::fs::write(path, content)
        .with_context(|| format!("Failed to write {}", path.display()))?;
    println!("  {} .pipeline/{}   created", "OK".green(), label);
    Ok(())
}

fn generate_role_files(repo_root: &Path, pipeline_dir: &Path, refresh: bool) -> Result<()> {
    println!("\n  Generating role standards files using Claude...");
    println!("  INFO: This may take 30-60 seconds");

    let claude_md = std::fs::read_to_string(repo_root.join("CLAUDE.md"))
        .unwrap_or_default();
    let tree = get_repo_structure(repo_root);

    let backend_path = pipeline_dir.join("backend.md");
    let frontend_path = pipeline_dir.join("frontend.md");

    if refresh || !backend_path.exists() {
        let backend_prompt = build_backend_prompt(&claude_md, &tree);
        match run_claude_setup(&backend_prompt, repo_root) {
            Ok(content) => {
                std::fs::write(&backend_path, &content)
                    .with_context(|| format!("Failed to write {}", backend_path.display()))?;
                println!("  {} .pipeline/backend.md    generated", "OK".green());
            }
            Err(e) => {
                println!("  {} .pipeline/backend.md    Claude failed -- writing template", "WARN".yellow());
                println!("    {}", e.to_string().dimmed());
                std::fs::write(&backend_path, BACKEND_MD_TEMPLATE)
                    .with_context(|| format!("Failed to write {}", backend_path.display()))?;
            }
        }
    }

    if refresh || !frontend_path.exists() {
        let frontend_prompt = build_frontend_prompt(&claude_md, &tree);
        match run_claude_setup(&frontend_prompt, repo_root) {
            Ok(content) => {
                std::fs::write(&frontend_path, &content)
                    .with_context(|| format!("Failed to write {}", frontend_path.display()))?;
                println!("  {} .pipeline/frontend.md   generated", "OK".green());
            }
            Err(e) => {
                println!("  {} .pipeline/frontend.md   Claude failed -- writing template", "WARN".yellow());
                println!("    {}", e.to_string().dimmed());
                std::fs::write(&frontend_path, FRONTEND_MD_TEMPLATE)
                    .with_context(|| format!("Failed to write {}", frontend_path.display()))?;
            }
        }
    }

    Ok(())
}

fn build_backend_prompt(claude_md: &str, tree: &str) -> String {
    format!(
        "{claude_md}\n\n---\n\n\
You are analyzing this repository to generate backend coding standards for an autonomous pipeline.\n\
The repository structure is:\n{tree}\n\n\
Write a concise backend.md file (200-400 words) covering:\n\
1. Language/framework and key architectural patterns\n\
2. File organization conventions (where new files go, naming)\n\
3. How to run tests for backend code (exact commands)\n\
4. Key quality rules (error handling patterns, logging, etc.)\n\
5. Common pitfalls to avoid in this codebase\n\
6. Any PUSH CONTRACT rules (what must pass before committing)\n\n\
Focus on patterns that actually exist in THIS codebase -- no generic advice.\n\
Output only the markdown content, starting with: # Backend Standards\n",
        claude_md = claude_md,
        tree = tree,
    )
}

fn build_frontend_prompt(claude_md: &str, tree: &str) -> String {
    format!(
        "{claude_md}\n\n---\n\n\
You are analyzing this repository to generate frontend coding standards for an autonomous pipeline.\n\
The repository structure is:\n{tree}\n\n\
Write a concise frontend.md file (200-400 words) covering:\n\
1. Framework and key UI patterns\n\
2. File organization conventions\n\
3. How to run frontend tests (exact commands)\n\
4. Key quality rules (component patterns, state management, etc.)\n\
5. Common pitfalls to avoid\n\
6. Any PUSH CONTRACT rules\n\n\
Focus on patterns that actually exist in THIS codebase.\n\
Output only the markdown content, starting with: # Frontend Standards\n",
        claude_md = claude_md,
        tree = tree,
    )
}

fn run_claude_setup(prompt: &str, cwd: &Path) -> Result<String> {
    let out = Command::new("claude")
        .args(["-p", "--max-budget-usd", "0.50", prompt])
        .current_dir(cwd)
        .output()
        .context("Failed to run claude")?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        bail!("claude exited with error: {}", stderr.trim());
    }

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if stdout.trim().is_empty() {
        bail!("claude returned empty output");
    }

    Ok(stdout.trim().to_string())
}

fn get_repo_structure(repo_root: &Path) -> String {
    let out = Command::new("find")
        .args([".", "-maxdepth", "3", "-type", "f",
               "-name", "*.rs",
               "-o", "-type", "f", "-name", "*.ts",
               "-o", "-type", "f", "-name", "*.tsx",
               "-o", "-type", "f", "-name", "Cargo.toml"])
        .current_dir(repo_root)
        .output();

    match out {
        Ok(o) if o.status.success() => {
            let text = String::from_utf8_lossy(&o.stdout);
            let lines: Vec<&str> = text.lines().take(60).collect();
            lines.join("\n")
        }
        _ => "(could not read repo structure)".to_string(),
    }
}

fn ensure_gitignore(repo_root: &Path) -> Result<()> {
    let gitignore = repo_root.join(".gitignore");
    let entry = "PIPELINE/";

    if gitignore.exists() {
        let content = std::fs::read_to_string(&gitignore).unwrap_or_default();
        if content.contains(entry) {
            return Ok(());
        }
        let updated = format!("{}\n# Pipeline runtime state\n{}\n", content.trim_end(), entry);
        std::fs::write(&gitignore, updated).context("Failed to update .gitignore")?;
        println!("  {} .gitignore       added PIPELINE/ entry", "OK".green());
    } else {
        std::fs::write(&gitignore, format!("# Pipeline runtime state\n{}\n", entry))
            .context("Failed to create .gitignore")?;
        println!("  {} .gitignore       created with PIPELINE/ entry", "OK".green());
    }

    Ok(())
}

const BACKEND_MD_TEMPLATE: &str = "# Backend Standards\n\n\
> Generated by `pipeline setup` -- customize this file for your repo conventions.\n\n\
## Language & Architecture\n\
- TODO: describe language, framework, key patterns\n\n\
## File Organization\n\
- TODO: describe where new files go, naming conventions\n\n\
## Testing\n\
```bash\n\
# TODO: add exact test commands\n\
cargo test\n\
```\n\n\
## Quality Rules\n\
- TODO: error handling patterns\n\
- TODO: logging conventions\n\n\
## PUSH CONTRACT\n\
Before committing, these must pass:\n\
- TODO: list required checks (tests, lints, etc.)\n\n\
## Common Pitfalls\n\
- TODO: describe codebase-specific gotchas\n";

const FRONTEND_MD_TEMPLATE: &str = "# Frontend Standards\n\n\
> Generated by `pipeline setup` -- customize this file for your repo conventions.\n\n\
## Framework & Patterns\n\
- TODO: describe framework, component patterns, state management\n\n\
## File Organization\n\
- TODO: where do new components/pages/styles go?\n\n\
## Testing\n\
```bash\n\
# TODO: add exact test commands\n\
npm test\n\
```\n\n\
## Quality Rules\n\
- TODO: accessibility requirements\n\
- TODO: performance budgets\n\n\
## PUSH CONTRACT\n\
Before committing:\n\
- TODO: list required checks\n\n\
## Common Pitfalls\n\
- TODO: describe frontend-specific gotchas\n";
