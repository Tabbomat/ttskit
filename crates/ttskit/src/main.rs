mod cloud;

use std::io::{IsTerminal, Write};
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
    /// Upload a file to the Steam Cloud, share it and print its URL
    Upload {
        /// The local file to upload
        file: PathBuf,
        /// Target cloud folder (separator `/`); default is the root
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// Name in the cloud; default is the file name
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Only print the plan
        #[arg(long, conflicts_with = "yes")]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
        /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
        #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
        backup_dir: Option<PathBuf>,
    },
    /// Delete a file or folder from the Steam Cloud (dry run unless --yes)
    #[command(group(clap::ArgGroup::new("what").required(true).args(["name", "folder"])))]
    Delete {
        /// Folder path and name (`Decks/back.png`), display name, or cloud name
        name: Option<String>,
        /// Delete this folder with all its files and subfolders (separator `/`)
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// Delete for real; without it only the plan is printed
        #[arg(long)]
        yes: bool,
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
        /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
        #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
        backup_dir: Option<PathBuf>,
    },
    /// Compare a local directory with a cloud folder (read-only)
    Status {
        /// The local directory; its subdirectories map to cloud subfolders
        dir: PathBuf,
        /// Cloud folder to compare with (separator `/`); default is the root
        #[arg(long, value_name = "FOLDER")]
        folder: Option<String>,
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
    },
    /// Print the download URL of a cloud file
    Url {
        /// Folder path and name (`Decks/back.png`), display name, or cloud name
        name: String,
        /// Share the file first if it has no URL yet
        #[arg(long)]
        share: bool,
        /// Only print the plan
        #[arg(long, conflicts_with = "yes")]
        dry_run: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
        /// Print machine-readable JSON instead of text
        #[arg(long)]
        json: bool,
        /// Directory for the index backups [default: <local app data>/ttskit/cloud-backups]
        #[arg(long, env = "TTSKIT_BACKUP_DIR", value_name = "DIR")]
        backup_dir: Option<PathBuf>,
    },
}

/// Asks on the terminal; the prompt goes to stderr so that stdout stays clean.
fn ask_on_terminal(prompt: &str) -> Result<bool, cloud::CommandError> {
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        return Err(cloud::CommandError::ConfirmationNeeded);
    }
    eprint!("{prompt} [y/N] ");
    std::io::stderr().flush()?;
    let mut line = String::new();
    stdin.read_line(&mut line)?;
    let answer = line.trim();
    Ok(answer.eq_ignore_ascii_case("y") || answer.eq_ignore_ascii_case("yes"))
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

fn run_simple(command: Command) -> Result<(), String> {
    match command {
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
        Command::Cloud {
            command:
                CloudCommand::Upload {
                    file,
                    folder,
                    name,
                    dry_run,
                    yes,
                    json,
                    backup_dir,
                },
        } => {
            // Check everything local before connecting to Steam.
            let (name, data) =
                cloud::read_upload_file(&file, name.as_deref()).map_err(|e| e.to_string())?;
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let mut backend = connect()?;
            let req = cloud::UploadRequest {
                name: &name,
                data: &data,
                folder: folder.as_deref().unwrap_or_default(),
                mode: cloud::RunMode { dry_run, yes, json },
            };
            let mut out = std::io::stdout().lock();
            cloud::upload_file(
                backend.as_mut(),
                &store,
                &req,
                SystemTime::now(),
                &cloud::format_date(&jiff::Zoned::now()),
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        Command::Cloud {
            command:
                CloudCommand::Delete {
                    name,
                    folder,
                    yes,
                    json,
                    backup_dir,
                },
        } => {
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let target = match (&name, &folder) {
                (Some(name), None) => ttskit_cloud::DeleteTarget::File(name),
                (None, Some(folder)) => ttskit_cloud::DeleteTarget::Folder(folder),
                _ => unreachable!("clap requires exactly one of name and --folder"),
            };
            let mut backend = connect()?;
            let mut out = std::io::stdout().lock();
            cloud::delete_files(
                backend.as_mut(),
                &store,
                target,
                yes,
                json,
                SystemTime::now(),
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        Command::Cloud {
            command:
                CloudCommand::Url {
                    name,
                    share,
                    dry_run,
                    yes,
                    json,
                    backup_dir,
                },
        } => {
            let store = BackupStore::new(cloud::backup_dir(backup_dir).map_err(|e| e.to_string())?);
            let mut backend = connect()?;
            let req = cloud::UrlRequest {
                query: &name,
                share,
                mode: cloud::RunMode { dry_run, yes, json },
            };
            let mut out = std::io::stdout().lock();
            cloud::show_url(
                backend.as_mut(),
                &store,
                &req,
                SystemTime::now(),
                &mut ask_on_terminal,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())
        }
        Command::Cloud {
            command: CloudCommand::Status { .. },
        } => unreachable!("handled by run"),
    }
}

/// Exit code of `cloud status`: 0 in sync, 2 otherwise.
fn status_exit_code(in_sync: bool) -> ExitCode {
    if in_sync {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}

/// Runs the command; the exit code is 0 unless the command says otherwise.
fn run(cli: Cli) -> Result<ExitCode, String> {
    match cli.command {
        Command::Cloud {
            command: CloudCommand::Status { dir, folder, json },
        } => {
            // Scan the local side before connecting to Steam.
            let local = ttskit_cloud::scan_local(&dir).map_err(|e| e.to_string())?;
            let backend = connect()?;
            let mut out = std::io::stdout().lock();
            let in_sync = cloud::status(
                backend.as_ref(),
                &local,
                folder.as_deref().unwrap_or_default(),
                &dir.display().to_string(),
                json,
                &mut out,
            )
            .map_err(|e| e.to_string())?;
            out.flush().map_err(|e| e.to_string())?;
            Ok(status_exit_code(in_sync))
        }
        command => run_simple(command).map(|()| ExitCode::SUCCESS),
    }
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("error: {message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(
            ["ttskit", "cloud", "delete"]
                .into_iter()
                .chain(args.iter().copied()),
        )
    }

    #[test]
    fn delete_needs_exactly_one_of_name_and_folder() {
        assert!(parse(&["a.png"]).is_ok());
        assert!(parse(&["--folder", "Decks", "--yes"]).is_ok());
        assert!(parse(&[]).is_err());
        assert!(parse(&["--yes"]).is_err());
        assert!(parse(&["a.png", "--folder", "Decks"]).is_err());
    }

    #[test]
    fn status_exit_codes() {
        assert_eq!(status_exit_code(true), ExitCode::SUCCESS);
        assert_eq!(status_exit_code(false), ExitCode::from(2));
    }
}
