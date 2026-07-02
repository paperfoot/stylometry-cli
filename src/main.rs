//! stylometry -- authorship verification CLI.
//!
//! Build per-author profiles by fingerprinting their writing, then verify new
//! text against them with calibrated Burrows/Cosine Delta. Local, single
//! static binary, agent-friendly (JSON envelope, semantic exit codes,
//! agent-info manifest). Built on the agent-cli-framework patterns.

mod cli;
mod commands;
mod config;
mod engine;
mod error;
mod output;

use clap::Parser;

use cli::{Cli, Commands, ConfigAction, ProfileAction, SkillAction};
use output::{Ctx, Format};

/// Pre-scan argv for --json before clap parses, so --json is honored even on
/// help, version, and parse-error paths where the Cli struct isn't populated.
fn has_json_flag() -> bool {
    std::env::args_os().any(|a| a == "--json")
}

fn main() {
    let json_flag = has_json_flag();

    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            if matches!(
                e.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let format = Format::detect(json_flag);
                match format {
                    Format::Json => {
                        output::print_help_json(e);
                        std::process::exit(0);
                    }
                    Format::Human => e.exit(),
                }
            }
            let format = Format::detect(json_flag);
            output::print_clap_error(format, &e);
            std::process::exit(3);
        }
    };

    let ctx = Ctx::new(cli.json, cli.quiet);

    let result = match cli.command {
        Commands::Profile { action } => match action {
            ProfileAction::Build {
                name,
                corpus,
                chunk_size,
                force,
            } => commands::profile::build(ctx, name, corpus, chunk_size, force),
            ProfileAction::List => commands::profile::list(ctx),
            ProfileAction::Show { name } => commands::profile::show(ctx, name),
            ProfileAction::Remove { name } => commands::profile::remove(ctx, name),
        },
        Commands::Compare {
            profile,
            file,
            text,
        } => commands::compare::run(ctx, profile, file, text),
        Commands::Calibrate { name } => commands::calibrate::run(ctx, name),
        Commands::AgentInfo => {
            commands::agent_info::run();
            Ok(())
        }
        Commands::Skill { action } => match action {
            SkillAction::Install => commands::skill::install(ctx),
            SkillAction::Status => commands::skill::status(ctx),
        },
        Commands::Config { action } => match action {
            ConfigAction::Show => config::load().and_then(|cfg| commands::config::show(ctx, &cfg)),
            ConfigAction::Path => commands::config::path(ctx),
        },
        Commands::Update { check } => {
            config::load().and_then(|cfg| commands::update::run(ctx, check, &cfg))
        }
        Commands::Contract { code } => commands::contract::run(ctx, code),
    };

    if let Err(e) = result {
        output::print_error(ctx.format, &e);
        std::process::exit(e.exit_code());
    }
}
