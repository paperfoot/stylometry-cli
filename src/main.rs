//! stylometry -- analyse a writer's voice, brief agents to write in it, and
//! check drafts against it.
//!
//! Build per-author voice profiles from their writing; report their habits
//! (signature and avoided words, synonym choices, rhythm, punctuation);
//! produce a style brief an agent can follow; check drafts line by line; and
//! verify authorship with calibrated Burrows/Cosine Delta. Local, single
//! binary, agent-friendly (JSON envelope, semantic exit codes, agent-info
//! manifest). Built on the agent-cli-framework patterns.

mod cli;
mod commands;
mod config;
mod engine;
mod error;
mod output;

use clap::Parser;

use cli::{BackgroundAction, Cli, Commands, ConfigAction, ProfileAction, SkillAction};
use output::{Ctx, Format};

/// Pre-scan argv for --json (up to the `--` delimiter) before clap parses, so
/// --json is honored even on help, version, and parse-error paths.
fn has_json_flag() -> bool {
    std::env::args_os()
        .take_while(|a| a != "--")
        .any(|a| a == "--json")
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
                if json_flag {
                    let code = match output::print_help_json(e) {
                        Ok(()) => 0,
                        Err(err) => {
                            output::print_error(Format::Json, &err);
                            err.exit_code()
                        }
                    };
                    std::process::exit(code);
                }
                e.exit();
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
                background,
                force,
            } => commands::profile::build(ctx, name, corpus, background, force),
            ProfileAction::Blend { name, from, force } => {
                commands::profile::blend(ctx, name, from, force)
            }
            ProfileAction::List { all } => commands::profile::list(ctx, all),
            ProfileAction::Show { name } => commands::profile::show(ctx, name),
            ProfileAction::Rebuild { name } => commands::profile::rebuild(ctx, name),
            ProfileAction::Remove { name, confirm } => {
                commands::profile::remove(ctx, name, confirm)
            }
        },
        Commands::Analyze {
            source,
            limit,
            window,
        } => commands::analyze::run(ctx, source, limit, window),
        Commands::Guide {
            profile,
            exemplars,
            limit,
            window,
            out,
        } => commands::guide::run(ctx, profile, exemplars, limit, window, out),
        Commands::Check {
            file,
            profile,
            text,
            limit,
            avoid,
            guard,
            strict,
        } => commands::check::run(
            ctx,
            commands::check::CheckArgs {
                profile,
                file,
                text,
                limit,
                avoid,
                guard,
                strict,
            },
        ),
        Commands::Export {
            profile,
            out,
            format,
        } => commands::export::run(ctx, profile, out, format),
        Commands::Diff { a, b, limit } => commands::diff::run(ctx, a, b, limit),
        Commands::Compare {
            profile,
            file,
            text,
        } => commands::compare::run(ctx, profile, file, text),
        Commands::Calibrate { profile } => commands::calibrate::run(ctx, profile),
        Commands::Background { action } => match action {
            BackgroundAction::Fetch { force } => commands::background::fetch(ctx, force),
            BackgroundAction::Status => commands::background::status(ctx),
        },
        Commands::AgentInfo { command } => {
            let _ = command;
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
        Commands::Doctor => commands::doctor::run(ctx),
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
