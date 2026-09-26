mod artifact_export;
mod chat_edit;
mod artifact_library;
mod attachments;
mod attention;
mod avatars;
#[cfg(test)]
mod bot_archive_tests;
mod bot_drafts;
mod bot_archive;
mod bot_instructions;
#[cfg(test)]
mod chat_history_tests;
#[cfg(test)]
mod chat_send_tests;
#[cfg(test)]
mod chat_tests;
mod chats;
mod cli_providers;
mod codex_connectors;
mod collaboration;
mod command_jobs;
mod commands;
mod composio;
mod config;
mod connections;
mod connector_artifacts;
mod connector_edits;
mod connector_policy;
mod connector_records;
mod continuity;
mod visual_panels;
mod workflow_panels;
mod conversation_updates;
mod db;
mod desktop_sessions;
#[cfg(test)]
mod decision_tests;
mod deliverables;
#[cfg(test)]
mod experience_tests;
mod gmail_push;
mod guest;
mod instructions;
mod local_access;
mod mail_watch;
mod managed_process;
mod message_actions;
#[cfg(test)]
#[path = "../desktop/src/notification_xml.rs"]
mod notification_xml;
mod notifications;
mod opencode;
mod pairing;
mod pi;
mod plans;
mod workspace_artifacts;
#[cfg(test)]
mod plans_tests;
mod profiles;
mod provider_accounts;
mod provider_catalog;
mod provider_inbox;
mod provider_retry;
mod providers;
mod questions;
mod quiet_output;
#[cfg(test)]
mod recovery_fairness_tests;
#[cfg(test)]
mod recovery_isolation_tests;
mod resources;
mod routine_controls;
#[cfg(test)]
mod routine_duplicate_tests;
mod routine_updates;
mod rpc;
mod runtime;
mod schedules;
mod screen_control;
#[cfg(test)]
#[path = "../desktop/src/session_handoff.rs"]
mod session_handoff;
#[cfg(test)]
mod settings_tests;
#[cfg(test)]
#[path = "../desktop/src/skill_files.rs"]
mod skill_files;
mod skill_import;
mod skill_refresh;
mod task_recovery;
mod team_chats;
#[cfg(test)]
mod team_restart_tests;
#[cfg(test)]
mod tests;
mod timezone;
mod uploads;
mod user_tasks;
mod vm;
mod vm_maintenance;
mod vnc;
mod web;
mod release_github;
mod server_update;
#[cfg(test)]
mod workflow_recovery_tests;
mod workspace_import;
mod workspace_transfer;

use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use std::{io::Write, path::PathBuf};

#[derive(Parser)]
#[command(version, about = "AI teammates on your own shared Linux VM")]
struct Cli {
    #[arg(long, default_value = "kindred.toml", global = true)]
    config: PathBuf,
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Write a starter config. Does not provision a VM or change system services.
    Init,
    /// Print a new random server access token.
    Token,
    /// Validate the config without contacting providers or the VM.
    Check,
    /// Start the personal server.
    Serve,
    /// Guest-only JSON stdin/stdout tool bridge. Install this binary inside the bot VM too.
    GuestRpc,
}
#[tokio::main]
async fn main() -> Result<()> {
    if managed_process::worker_argument() {
        return Ok(());
    }
    let cli = Cli::parse();
    match cli.command {
        Action::Init => {
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&cli.config)
                .context("config already exists or cannot be created")?;
            f.write_all(toml::to_string_pretty(&config::Config::default())?.as_bytes())?;
            println!(
                "Created {}. Run `kindred token` and set KINDRED_TOKEN before serving.",
                cli.config.display()
            );
        }
        Action::Token => println!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ),
        Action::Check => {
            config::Config::load(&cli.config)?;
            println!("Configuration is valid. Provider and VM connectivity were not tested.");
        }
        Action::GuestRpc => guest::rpc().await?,
        Action::Serve => {
            let config = config::Config::load(&cli.config)?;
            let listen = config.listen;
            let listener = tokio::net::TcpListener::bind(listen).await?;
            let legacy = !config.profiles.enabled || config.profiles.import_legacy;
            let app = if legacy {
                let token = std::env::var(&config.token_env)
                    .context("Set KINDRED_TOKEN before serving a legacy workspace")?;
                ensure!(
                    token.len() >= 32,
                    "Server access token must be at least 32 bytes"
                );
                Some(runtime::App::open(config.clone(), token)?)
            } else {
                None
            };
            let scheduler = app
                .as_ref()
                .map(|app| tokio::spawn(runtime::scheduler(app.clone())));
            if let Some(app) = &app {
                tokio::spawn(provider_catalog::startup(app.clone()));
            }
            let router = if config.profiles.enabled {
                profiles::router(profiles::Profiles::open(config.clone(), app, true)?)
            } else {
                web::router(app.unwrap())
            };
            println!(
                "Kindred {} listening at {}",
                env!("CARGO_PKG_VERSION"),
                config.public_url
            );
            axum::serve(listener, router)
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            if let Some(scheduler) = scheduler {
                scheduler.abort();
            }
        }
    }
    Ok(())
}
