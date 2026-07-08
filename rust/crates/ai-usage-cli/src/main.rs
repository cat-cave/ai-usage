//! `ai-usage` CLI binary. Owns rendering only; all logic lives in the library.

use std::process::ExitCode;

use ai_usage::{Registry, TaskKind};
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

mod render;

#[derive(Parser, Debug)]
#[command(
    name = "ai-usage",
    version,
    about = "Report AI coding-provider capacity and recommend a provider for a task."
)]
struct Cli {
    /// Emit machine-readable JSON (versioned schema).
    #[arg(long, global = true)]
    json: bool,

    /// Cache-only; do not hit the network. Every value is marked Stale/Cached.
    #[arg(long, global = true)]
    offline: bool,

    /// Force a live refresh; bypass cache.
    #[arg(long, global = true)]
    refresh: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Show one provider in detail.
    Provider { id: String },
    /// Rank providers for a task; explainable.
    Recommend {
        #[arg(long)]
        task: String,
    },
    /// Auth + freshness diagnostics (redacted).
    Doctor,
    /// Manage configuration.
    #[command(subcommand)]
    Config(ConfigCmd),
}

#[derive(Subcommand, Debug)]
enum ConfigCmd {
    /// Print the resolved config (secrets redacted).
    Show,
    /// Validate config + endpoint overrides (HTTPS-only).
    Validate,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!(
                "error: {}",
                ai_usage::redact::Redactor::redact_str(&e.to_string())
            );
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();

    let registry = Registry::from_env().map_err(|e| anyhow!(e.to_string()))?;
    let report = registry.snapshot().await;

    match &cli.command {
        None => emit(
            &cli,
            || render::render_table(&report),
            || render::render_json(&report),
        ),
        Some(Command::Provider { id }) => {
            let pid = parse_id(id)?;
            match report.find(pid) {
                Some(_) => {
                    // For the deep view we still print the table row; a richer
                    // per-provider detail renderer lands in a later phase.
                    let single = ai_usage::AggregateReport::new(
                        report
                            .providers
                            .iter()
                            .filter(|p| p.id == pid)
                            .cloned()
                            .collect(),
                    );
                    emit(
                        &cli,
                        || render::render_table(&single),
                        || render::render_json(&single),
                    )
                }
                None => Err(anyhow!("unknown provider: {id}")),
            }
        }
        Some(Command::Recommend { task }) => {
            let kind = TaskKind::parse(task).ok_or_else(|| {
                anyhow!(
                "unknown task '{task}'; try short|long-coding|exploratory|review|high-context|audit"
            )
            })?;
            let recs = report.recommend(kind);
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&recs)?);
            } else {
                print!("{}", render::render_recommendations(&recs, kind));
            }
            Ok(())
        }
        Some(Command::Doctor) => emit(
            &cli,
            || render::render_doctor(&report),
            || render::render_json(&report),
        ),
        Some(Command::Config(ConfigCmd::Show)) => {
            let cfg = ai_usage::config::Config::load().map_err(|e| anyhow!(e.to_string()))?;
            println!("{}", serde_json::to_string_pretty(&cfg)?);
            Ok(())
        }
        Some(Command::Config(ConfigCmd::Validate)) => {
            // Loading already validates structure; HTTPS overrides are checked at
            // provider construction. Re-construct each provider to surface errors.
            let cfg = ai_usage::config::Config::load().map_err(|e| anyhow!(e.to_string()))?;
            let _ = ai_usage::providers::openrouter::OpenRouterProvider::from_config(&cfg)
                .map_err(|e| anyhow!(e.to_string()))?;
            println!("config valid");
            Ok(())
        }
    }
}

fn parse_id(s: &str) -> Result<ai_usage::ProviderId> {
    Ok(match s.to_ascii_lowercase().as_str() {
        "codex" => ai_usage::ProviderId::Codex,
        "claude" => ai_usage::ProviderId::Claude,
        "zai" | "z.ai" => ai_usage::ProviderId::Zai,
        "minimax" => ai_usage::ProviderId::MiniMax,
        "openrouter" => ai_usage::ProviderId::OpenRouter,
        "deepseek" => ai_usage::ProviderId::DeepSeek,
        "grok" => ai_usage::ProviderId::Grok,
        _ => return Err(anyhow!("unknown provider: {s}")),
    })
}

fn emit<F1, F2>(cli: &Cli, human: F1, json: F2) -> Result<()>
where
    F1: FnOnce() -> String,
    F2: FnOnce() -> anyhow::Result<String>,
{
    if cli.json {
        println!("{}", json()?);
    } else {
        print!("{}", human());
    }
    Ok(())
}
