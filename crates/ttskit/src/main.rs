mod cloud;

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::SystemTime;

use clap::{Parser, Subcommand};
use ttskit_cloud::{BackupStore, CloudBackend};

/// Unofficial command line toolkit for Tabletop Simulator mod development.
#[derive(Parser)]
#[command(version, about, arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage the Steam Cloud files of Tabletop Simulator (unofficial)
    Cloud {
        #[command(subcommand)]
        command: CloudCommand,
    },
}

#[derive(Subcommand)]
enum CloudCommand {
    /// List the folders and files in the Steam Cloud (read-only)
    List {
        /// Only show this folder and its subfolders (separator `/`)
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// Print machine-readable JSON instead of a tree
        #[arg(long)]
        json: bool,
    },
    /// Restore the cloud index from a local backup (dry run unless --yes)
    Restore {
        /// Backup to restore; omit to list the backups
        backup: Option<String>,
        /// Restore for real; without it only the plan is printed
        #[arg(long)]
        yes: bool,
        /// Directory holding the backups [default: <local app data>/ttskit/cloud-backups]
        #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
        backup_dir: Option<PathBuf>,
    },
}

#[cfg(feature = "steam")]
fn connect() -> Result<Box<dyn CloudBackend>, String> {
    ttskit_cloud::SteamCloud::connect()
        .map(|cloud| Box::new(cloud) as Box<dyn CloudBackend>)
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "steam"))]
fn connect() -> Result<Box<dyn CloudBackend>, String> {
    Err(
        "this ttskit build has no Steam support; rebuild with `cargo build --features steam`"
            .to_owned(),
    )
}

fn run(cli: Cli) -> Result<(), String> {
    match cli.command {
        Command::Cloud {
            command: CloudCommand::List { folder, json },
        } => {
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::list(backend.as_ref(), folder.as_deref(), json, &mut out)
                .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        Command::Cloud {
            command:
                CloudCommand::Restore {
                    backup,
                    yes,
                    backup_dir,
                },
        } => {
            let dir = cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?;
            let store = BackupStore::new(dir);
            let mut out = std::io::stdout().lock();
            match backup {
                None => cloud::list_backups(&store, &mut out).map_err(|e| e.to_string())?,
                Some(name) => {
                    // Validate the name before connecting to Steam.
                    let backup = store.get(&name).map_err(|e| e.to_string())?;
                    let mut backend = connect()?;
                    cloud::restore_backup(
                        backend.as_mut(),
                        &store,
                        &backup,
                        yes,
                        SystemTime::now(),
                        &mut out,
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            out.flush().map_err(|e| e.to_string())
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}
